# Windows NuGet package

## Installation

`NaviApps.SoftwareLicensing` targets .NET 10 and supplies a `win-x64` native
asset. The maintainer distribution workflow checks the package on Windows x64. On Windows 11
ARM64 (build 26100 or later), select x64 .NET to run consumers under emulation.

Use the [Windows package acceptance command](#windows-package-acceptance) to
produce a local package and `software-licensing-windows.json` receipt. Verify the
package's SHA-256 against the receipt and use its directory as a local NuGet source.

Set `DOTNET_BIN` to the absolute x64 .NET path. In a new consumer directory,
use a fresh package cache and replace the example source with the directory
containing the verified package:

```powershell
$env:NUGET_PACKAGES = Join-Path (Get-Location) ".nuget-packages"
& "$env:DOTNET_BIN" new console --framework net10.0
& "$env:DOTNET_BIN" add package NaviApps.SoftwareLicensing --version 0.1.0 --source C:\owned\packages
```

Copy the [standalone .NET example](../../fixtures/dotnet-consumer/Program.cs) to
`Program.cs`, then run `& "$env:DOTNET_BIN" run --runtime win-x64 -p:UseAppHost=false`.
The same example runs against the isolated NuGet package in distribution checks.
It uses synthetic access and in-memory storage to activate, reopen, refresh and deactivate.

For application integration, use `CallbackLicenseProvider` with a read-only
access check or supply an `ILicenseProvider` for key-based licensing. Select an
`ILicenseStorage` and create a `LicenseManager`. Windows hosts can use
`WindowsLicenseStorage`; each manager needs its own private absolute directory
and stable namespace. Inspect `InitialRestoreError` before starting mutations.
The callback provider preserves typed failures and marks them `NotApplied`.
The manager clears failed activation intents so the user can retry.

Use `LicenseSource.NormalizeIdentifier`, `LicenseActivation.CreateValidated`,
`LicenseValidationResult.CreateValidated` and `LicenseRefreshPolicy.CreateValidated`
to check or normalize values before lifecycle operations. These calls use the
installed native library and throw `LicenseException` for invalid values.
The public record constructors still store the supplied values, and the
manager validates those values when it processes them.

`Plan` projects the current state. `NeedsRefresh()` uses the manager's clock,
and `NeedsRefresh(now)` uses the supplied observed time for refresh eligibility
and rollback protection. The query makes no provider or storage calls.
The [lifecycle and recovery guide](https://github.com/naviapps/software-licensing#readme)
defines access, refresh scheduling and persistence recovery for all bindings.
The [managed fixtures](tests/SoftwareLicensing.Tests/Program.cs) exercise these
APIs with synthetic provider results. Ordinary package consumers do not need
Rust or Python developer tooling.

## Maintainer build

### Windows package acceptance

From the SDK root, follow the
[distribution setup](../../README.md#distribution-checks)
with a clean committed checkout on an owned Windows 11 ARM64 worker. Prepare
native ARM64 Python and the pinned ARM64 Rust toolchain with the
`x86_64-pc-windows-msvc` standard library. Select `HostARM64\x64` with
`VsDevCmd.bat -host_arch=arm64 -arch=x64` and set `DOTNET_BIN` to the absolute
path of the pinned x64 .NET SDK. Install the SDK's pinned uv and Python with mise.

```powershell
mise run sdk-tooling -- python fixtures/distribution/windows.py --architecture x64
```

This verifies a single DLL across C, .NET recovery and DPAPI fixtures, and the
isolated NuGet consumer. The package and receipt are retained together under
`.native-build/win-x64/<version>/`. Point local package restore at that directory.
When `VM_RUNNER_ARTIFACTS_DIR` is supplied, the same files are also retained there
under `win-x64/<version>/`.
The receipt records the SDK and shared build helper hashes. The GitHub-hosted
distribution job runs on x64. On Windows ARM64, run the local command to verify x64
emulation.

### macOS checks

`mise exec -- make test-dotnet` on macOS verifies the tracked
framework and runs managed lifecycle fixtures against it. It exercises fake
providers and in-memory storage. Windows x64 validation exercises DPAPI.
