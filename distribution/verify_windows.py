#!/usr/bin/env python3
"""Validate the Windows x64 SDK and independent NuGet consumer on x64 or ARM64."""

import argparse
import hashlib
import os
import platform
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

from sdk_tooling.adapters import load, support_inputs
from sdk_tooling.native_plan import builder

from distribution.nuget import PACKAGE, check_package

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-pc-windows-msvc"
RID = "win-x64"
RECEIPT = "software-licensing-windows.json"


def acceptance_source():
    return load("windows_delivery").acceptance_source(ROOT, match_actions=True)


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def publish_x64(package, receipt, *, output_dir, artifact_root, tooling):
    load("windows_delivery").publish_x64(
        package,
        receipt,
        receipt_name=RECEIPT,
        output_dir=output_dir,
        artifact_root=artifact_root,
        tooling=tooling,
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--architecture", choices=("x64",), default="x64")
    parser.parse_args(argv)
    if sys.platform != "win32" or sys.getwindowsversion().build < 26100:
        raise ValueError("Windows 11 build 26100 or later is required for the DPAPI fixture")
    source = acceptance_source()
    tool_inputs = support_inputs(ROOT)
    windows = load("windows")
    managed = load("dotnet")
    artifacts = load("artifacts")
    environment = {
        key.upper(): value for key, value in os.environ.items() if key.upper() != "DOTNET_HOST_PATH"
    }
    host = windows.host_architecture()
    env, tools, dotnet = windows.native_environment(host, environment, target_architecture="x64")
    artifact_root = load("vm_environment").artifacts_dir(env)
    env["RUSTUP_AUTO_INSTALL"] = "0"
    sdk_version = subprocess.check_output(
        [str(dotnet), "--version"], cwd=ROOT, env=env, text=True
    ).strip()
    if sdk_version != tomllib.loads((ROOT / "mise.toml").read_text())["tools"]["dotnet"]:
        raise ValueError("Use the pinned x64 .NET SDK version from mise.toml")
    native = builder(ROOT, tooling=load)

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
                artifacts.stage_native(built / name, candidate / name) for name in names
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
        load("c_compiler").compile_c(
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
        managed.run_managed(
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
        packed_dll = artifacts.stage_native(dll, assets / RID / "native" / dll.name)
        packages = candidate / "packages"
        managed.pack_project(
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
            load("nuget"),
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
        if tool_inputs != support_inputs(ROOT):
            raise ValueError("Windows x64 build tools changed during acceptance")
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
            "toolInputs": tool_inputs,
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
        publish_x64(
            package, receipt, output_dir=output, artifact_root=artifact_root, tooling=artifacts
        )
    print(f"Windows x64 SoftwareLicensing acceptance passed on {host}.")


if __name__ == "__main__":
    main()
