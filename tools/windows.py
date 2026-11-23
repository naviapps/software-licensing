#!/usr/bin/env python3
"""Validate the Windows x64 SDK and independent NuGet consumer on x64 or ARM64."""

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from contextlib import ExitStack
from pathlib import Path

from _tooling import load_candidate, verify
from dotnet_consumer import PACKAGE, check_package

ROOT = Path(__file__).resolve().parent.parent
TARGET = "x86_64-pc-windows-msvc"
RID = "win-x64"
RECEIPT = "software-licensing-windows.json"


def acceptance_source():
    commit = os.environ.get("NAVI_CI_SOURCE_COMMIT")
    fingerprint = os.environ.get("NAVI_CI_SOURCE_FINGERPRINT")
    if commit is not None or fingerprint is not None:
        if not re.fullmatch(r"[a-f0-9]{64}", fingerprint or ""):
            raise ValueError("Captured Windows x64 acceptance requires the workspace SHA-256")
    else:
        if subprocess.check_output(
            ["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=ROOT, text=True
        ).strip():
            raise ValueError("Windows x64 acceptance requires a clean committed checkout")
        commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if not re.fullmatch(r"[a-f0-9]{40}(?:[a-f0-9]{24})?", commit or ""):
        raise ValueError("Windows x64 acceptance requires the full source commit SHA")
    if os.environ.get("GITHUB_SHA", commit) != commit:
        raise ValueError("Windows x64 acceptance source differs from the Actions revision")
    return commit, fingerprint


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def publish_x64(package, receipt, *, output_dir, artifact_root, tooling):
    relative = Path(RID) / receipt["version"]
    local = output_dir / relative
    external = artifact_root / relative if artifact_root else local
    local_path, external_path = local.resolve(), external.resolve()
    destinations = [local]
    if external_path != local_path:
        if external_path.is_relative_to(local_path) or local_path.is_relative_to(external_path):
            raise ValueError("Local and CI x64 evidence directories must be disjoint")
        destinations.append(external)

    names = {package.name, RECEIPT}

    def check_destination(path):
        if (
            path.parent.is_symlink()
            or path.parent.is_junction()
            or path.is_symlink()
            or path.is_junction()
            or (
                path.exists()
                and (
                    not path.is_dir()
                    or any(
                        name.name not in names or name.is_symlink() or not name.is_file()
                        for name in path.iterdir()
                    )
                )
            )
        ):
            raise ValueError("Windows x64 evidence directory contains unrelated files")

    def verify_pair(path):
        if (
            {entry.name for entry in path.iterdir()} != names
            or json.loads((path / RECEIPT).read_bytes()) != receipt
            or sha256(path / package.name) != receipt["packages"][package.name]
        ):
            raise ValueError("Windows x64 evidence differs from the verified package and receipt")

    with tooling.build_lock(output_dir), ExitStack() as staging:
        stages = []
        for destination in destinations:
            check_destination(destination)
            destination.parent.mkdir(parents=True, exist_ok=True)
            directory = staging.enter_context(
                tempfile.TemporaryDirectory(dir=destination.parent, prefix="evidence-")
            )
            stage = Path(directory) / destination.name
            stage.mkdir()
            shutil.copyfile(package, stage / package.name)
            tooling.write_json(stage / RECEIPT, receipt)
            verify_pair(stage)
            stages.append(stage)

        cleanup_error = None

        def verify_local():
            nonlocal cleanup_error
            verify_pair(local)
            if len(stages) == 1:
                return
            check_destination(external)
            external_verified = False

            def verify_external():
                nonlocal external_verified
                verify_pair(external)
                external_verified = True

            # Keep local rollback available until the CI pair is verified.
            try:
                tooling.publish(stages[1], external, verify_external)
            except BaseException as error:
                if not external_verified:
                    raise
                verify_pair(external)
                cleanup_error = error

        check_destination(local)
        tooling.publish(stages[0], local, verify_local)
        if cleanup_error is not None:
            raise cleanup_error


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--architecture", choices=("x64",), default="x64")
    parser.add_argument(
        "--tooling-source", type=Path, required=True, help="Local reviewed SDK tooling checkout"
    )
    parser.add_argument(
        "--tooling-revision", required=True, help="Exact Git commit of the local SDK tooling"
    )
    args = parser.parse_args(argv)
    if sys.platform != "win32" or sys.getwindowsversion().build < 26100:
        raise ValueError("Windows 11 build 26100 or later is required for the DPAPI fixture")
    source = acceptance_source()
    selected = load_candidate(args.tooling_source, args.tooling_revision)
    windows = selected("windows")
    bindings = selected("bindings")
    artifacts = selected("artifacts")
    environment = {
        key.upper(): value for key, value in os.environ.items() if key.upper() != "DOTNET_HOST_PATH"
    }
    host = windows.host_architecture()
    env, tools, dotnet = windows.native_environment(host, environment, target_architecture="x64")
    env["RUSTUP_AUTO_INSTALL"] = "0"
    sdk_version = subprocess.check_output(
        [str(dotnet), "--version"], cwd=ROOT, env=env, text=True
    ).strip()
    if sdk_version != tomllib.loads((ROOT / "mise.toml").read_text())["tools"]["dotnet"]:
        raise ValueError("Use the pinned x64 .NET SDK version from mise.toml")
    native = selected("native").NativeBuilder(
        ROOT,
        symbol_prefix="software_licensing_",
        minimum_macos="26.0",
        specification="tools/windows.py",
    )

    def run(*args):
        subprocess.run([str(a) for a in args], cwd=ROOT, env=env, check=True)

    run("cargo", "fetch", "--locked", "--target", TARGET)
    run("cargo", "fmt", "--all", "--", "--check")
    built = windows.build_library(native, env, tools, "x64", native_host=host)
    run(
        "cargo",
        "clippy",
        "--workspace",
        "--all-targets",
        "--target",
        TARGET,
        "--locked",
        "--offline",
        "--",
        "-D",
        "warnings",
    )
    run("cargo", "test", "--workspace", "--target", TARGET, "--locked", "--offline")

    output = ROOT / ".native-build"
    output.mkdir(exist_ok=True)
    toolchain = {"backend": "msvc", "host": windows.MSVC_TARGETS[host], "target": TARGET}
    with tempfile.TemporaryDirectory(dir=output, prefix="win-x64-candidate-") as directory:
        candidate = Path(directory)
        names = ("software_licensing.dll", "software_licensing.dll.lib")
        with artifacts.build_lock(output):
            verified, native_receipt = windows.verify_artifact(native, "x64", environment=env)
            if verified != built or native_receipt.get("toolchain") != toolchain:
                raise ValueError("Windows x64 library differs from the selected host build")
            receipt_digest = sha256(built / "native-build.json")
            dll, import_library = [
                bindings.stage_native(built / name, candidate / name) for name in names
            ]
            expected = {name: native_receipt["outputs"][name] for name in names}

        def verify_staged():
            if artifacts.hashes([dll, import_library], candidate) != expected:
                raise ValueError("Staged DLL or import library differs from its verified receipt")

        verify_staged()
        exports = windows.verify_library(
            dll,
            [ROOT / "crates/ffi/include/software_licensing.h"],
            "software_licensing_",
            tools,
            env,
            "x64",
        )
        executable = candidate / "abi.exe"
        # Windows prefers the executable directory for imported libraries.
        bindings.compile_c(
            ROOT / "crates/ffi/tests/c/abi.c",
            output=executable,
            compiler=[tools / "cl.exe"],
            include_dirs=[ROOT / "crates/ffi/include"],
            libraries=[import_library],
            style="msvc",
            cwd=ROOT,
            env=env,
        )
        windows.require_pe_machine(executable, "x64")
        run(executable)
        env["SOFTWARE_LICENSING_NATIVE_LIBRARY"] = str(dll)
        config = ROOT / "bindings/dotnet/NuGet.Config"
        bindings.run_managed(
            dotnet,
            ROOT / "bindings/dotnet/tests/SoftwareLicensing.Tests/SoftwareLicensing.Tests.csproj",
            cwd=ROOT,
            env=env,
            verify_style=True,
            runtime=RID,
            properties={"UseAppHost": "false"},
            nuget_config=config,
        )

        verify_staged()
        assets = candidate / "runtimes"
        packed_dll = bindings.stage_native(dll, assets / RID / "native" / dll.name)
        packages = candidate / "packages"
        bindings.pack_project(
            dotnet,
            ROOT / f"bindings/dotnet/src/{PACKAGE}/{PACKAGE}.csproj",
            output=packages,
            cwd=ROOT,
            env=env,
            properties={"SoftwareLicensingNativeAssets": str(assets)},
            nuget_config=config,
        )
        package = packages / f"{PACKAGE}.{native.version}.nupkg"
        package_hash = check_package(
            bindings,
            dotnet,
            root=ROOT,
            package=package,
            version=native.version,
            rid=RID,
            library=dll,
            output=candidate,
            env=env,
            exclude_paths=(ROOT / "target", built, candidate),
        )
        verify_staged()
        if sha256(packed_dll) != expected[dll.name]:
            raise ValueError("Packaged DLL staging changed during acceptance")
        if source != acceptance_source():
            raise ValueError("Windows x64 acceptance source changed")
        verify(args.tooling_source, args.tooling_revision)
        commit, fingerprint = source
        receipt = {
            "source": commit,
            "workspaceFingerprint": fingerprint,
            "target": TARGET,
            "runtime": RID,
            "os": platform.platform(),
            "execution": "x64-native" if host == "x64" else "x64-emulated-on-arm64",
            "version": native.version,
            "toolchain": toolchain,
            "sdkToolingCommit": args.tooling_revision,
            "dotnetVersion": sdk_version,
            "run": env.get("GITHUB_RUN_ID"),
            "attempt": env.get("GITHUB_RUN_ATTEMPT"),
            "exports": sorted(exports),
            "nativeBuildReceiptSha256": receipt_digest,
            "acceptance": [
                "Rust lifecycle and access mapping",
                "x64 C ABI",
                ".NET lifecycle and current-record recovery",
                "user-scoped DPAPI",
                "clean x64 NuGet lifecycle and persisted reopen",
            ],
            "packages": {package.name: package_hash},
            "dllSha256": expected[dll.name],
        }
        artifact_root = Path(env["NAVI_CI_ARTIFACTS"]) if env.get("NAVI_CI_ARTIFACTS") else None
        publish_x64(
            package, receipt, output_dir=output, artifact_root=artifact_root, tooling=artifacts
        )
    print(f"Windows x64 SoftwareLicensing acceptance passed on {host}.")


if __name__ == "__main__":
    main()
