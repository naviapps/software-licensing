#!/usr/bin/env python3
"""Run source and isolated NuGet licensing fixtures against the macOS artifact."""

import os
import platform
import shutil
import tempfile
from pathlib import Path

from sdk_tooling.adapters import load
from sdk_tooling.native_plan import builder

from distribution.nuget import PACKAGE, check_package


def main():
    root = Path(__file__).resolve().parents[1]
    env = dict(
        os.environ,
        DOTNET_CLI_TELEMETRY_OPTOUT="1",
        DOTNET_NOLOGO="1",
        DOTNET_CLI_USE_MSBUILD_SERVER="0",
        MSBUILDDISABLENODEREUSE="1",
    )
    dotnet = env.get("DOTNET_BIN") or shutil.which("dotnet", path=env.get("PATH"))
    if not dotnet:
        raise SystemExit("Install .NET 10 or provide DOTNET_BIN")
    managed = load("dotnet")
    managed.run_managed(
        dotnet,
        root / "bindings/dotnet/tests/SoftwareLicensing.Tests/SoftwareLicensing.Tests.csproj",
        cwd=root,
        env=env,
        configuration="Debug",
        nuget_config=root / "bindings/dotnet/NuGet.Config",
        verify_style=True,
    )
    native = builder(root)
    framework = (
        native.artifact / "macos-arm64_x86_64" / (native.module + ".framework") / native.module
    )
    rid = "osx-arm64" if platform.machine() in {"arm64", "aarch64"} else "osx-x64"
    output = root / ".native-build"
    output.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="dotnet-package-", dir=output) as directory:
        work = Path(directory)
        assets = work / "runtimes"
        library = load("artifacts").stage_native(
            framework, assets / rid / "native" / "libsoftware_licensing.dylib"
        )
        managed.pack_project(
            dotnet,
            root / f"bindings/dotnet/src/{PACKAGE}/{PACKAGE}.csproj",
            env=env,
            cwd=root,
            output=work,
            properties={"SoftwareLicensingNativeAssets": str(assets)},
            nuget_config=root / "bindings/dotnet/NuGet.Config",
        )
        check_package(
            load("nuget"),
            dotnet,
            root=root,
            package=work / f"{PACKAGE}.{native.version}.nupkg",
            version=native.version,
            rid=rid,
            library=library,
            output=work,
            env=env,
            exclude_paths=(root / "target", framework.parent, assets),
        )


if __name__ == "__main__":
    main()
