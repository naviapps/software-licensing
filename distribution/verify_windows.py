#!/usr/bin/env python3
"""Validate the Windows x64 SDK and independent NuGet consumer on x64 or ARM64."""

import argparse
import platform
import sys
import tempfile
from pathlib import Path

from sdk_tooling.adapters import load, support_inputs
from sdk_tooling.artifacts import file_sha256 as sha256
from sdk_tooling.native_plan import ffi_product
from sdk_tooling.windows_delivery import RID, TARGET, X64Acceptance, publish_x64

from distribution.nuget import PACKAGE, check_package

ROOT = Path(__file__).resolve().parents[1]
RECEIPT = "software-licensing-windows.json"


def acceptance_source():
    return load("windows_delivery").acceptance_source(ROOT, match_actions=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--architecture", choices=("x64",), default="x64")
    parser.add_argument(
        "--native-aot", action="store_true", help="Publish and run the win-x64 Native AOT consumer"
    )
    args = parser.parse_args(argv)
    if sys.platform != "win32" or sys.getwindowsversion().build < 26100:
        raise ValueError("Windows 11 build 26100 or later is required for the DPAPI fixture")
    source = acceptance_source()
    tool_inputs = support_inputs(ROOT)
    windows = load("windows")
    managed = load("dotnet")
    artifacts = load("artifacts")
    acceptance = X64Acceptance(ROOT)
    env, dotnet = acceptance.env, acceptance.dotnet
    artifact_root = load("vm_environment").artifacts_dir(env)
    native = ffi_product(ROOT, factory=load("ffi").FfiProduct)
    built = acceptance.build(native)
    output = ROOT / ".native-build"
    output.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output, prefix="win-x64-candidate-") as directory:
        candidate = Path(directory)
        with artifacts.build_lock(output):
            dll, import_library, native_receipt = acceptance.stage(native, built, candidate)
            receipt_digest = sha256(built / "native-build.json")
        expected = {
            path.name: native_receipt["outputs"][path.name] for path in (dll, import_library)
        }

        def verify_staged():
            if artifacts.hashes([dll, import_library], candidate) != expected:
                raise ValueError("Staged DLL or import library differs from its verified receipt")

        exports = windows.verify_library(
            dll,
            [ROOT / "crates/ffi/include/software_licensing.h"],
            "software_licensing_",
            acceptance.tools,
            env,
            "x64",
        )
        acceptance.run_c(
            ROOT / "crates/ffi/tests/c/abi.c",
            include_directory=ROOT / "crates/ffi/include",
            import_library=import_library,
        )
        config = ROOT / "bindings/dotnet/NuGet.Config"
        managed.run_managed(
            dotnet,
            ROOT / "bindings/dotnet/tests/SoftwareLicensing.Tests/SoftwareLicensing.Tests.csproj",
            cwd=ROOT,
            env=env,
            verify_style=True,
            runtime=RID,
            properties={"UseAppHost": "false", "SoftwareLicensingNativeLibrary": str(dll)},
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
            native_aot=args.native_aot,
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
            "execution": "x64-native" if acceptance.host == "x64" else "x64-emulated-on-arm64",
            "version": native.version,
            "toolchain": acceptance.toolchain,
            "toolInputs": tool_inputs,
            "dotnetVersion": acceptance.dotnet_version,
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
        if args.native_aot:
            receipt["nativeAotFixtureExecuted"] = True
        publish_x64(
            package,
            receipt,
            receipt_name=RECEIPT,
            output_dir=output,
            artifact_root=artifact_root,
            tooling=artifacts,
        )
    print(f"Windows x64 SoftwareLicensing acceptance passed on {acceptance.host}.")


if __name__ == "__main__":
    main()
