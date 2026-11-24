#!/usr/bin/env python3
"""Package SoftwareLicensing using verified native runtime artifacts."""

import argparse
from pathlib import Path

from sdk_tooling.native_plan import ffi_product
from sdk_tooling.nuget import pack_native
from sdk_tooling.windows_delivery import RID
from sdk_tooling.xcframework import xcframework

from distribution.nuget import PACKAGE, package_files

ROOT = Path(__file__).resolve().parents[1]
RUNTIMES = ("osx-arm64", "osx-x64", RID)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--runtime", action="append", required=True, choices=RUNTIMES)
    parser.add_argument("--result", type=Path, help="Write the verified package filename")
    args = parser.parse_args(argv)
    native = ffi_product(ROOT) if set(args.runtime) == {RID} else xcframework(ROOT)
    package = pack_native(
        native,
        project=ROOT / "bindings/dotnet/src" / PACKAGE / (PACKAGE + ".csproj"),
        package_files=package_files(ROOT),
        nuget_config=ROOT / "bindings/dotnet/NuGet.Config",
        input_files=[
            Path(__file__),
            ROOT / "distribution/nuget.py",
            ROOT / "native.toml",
            ROOT / "mise.toml",
        ],
        runtimes=args.runtime,
    )
    if args.result is not None:
        with args.result.open("x", encoding="utf-8") as result:
            result.write(package.name)


if __name__ == "__main__":
    main()
