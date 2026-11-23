# SoftwareLicensing for .NET

## Installation

`NaviApps.SoftwareLicensing` targets .NET 10 and supplies native assets for
macOS 26 or later on ARM64 or Intel, Windows x64, and Windows 11 ARM64 through x64
emulation (build 26100 or later). Choose the .NET SDK that matches your macOS
architecture, or the x64 .NET SDK on Windows. Check the architecture with
`dotnet --info`.

Download the combined NuGet artifact from an exact-revision
[distribution check](../../README.md#distribution-checks). Verify the package's
SHA-256 against the accompanying `.nupkg.json` receipt and use its directory
as a local NuGet source.

In an empty consumer directory, select the exact package version and replace
`C:\owned\packages` with the directory containing the verified package.

```powershell
dotnet new console --framework net10.0 --no-restore
dotnet package add NaviApps.SoftwareLicensing --version "[0.1.0]" --no-restore
dotnet restore --source "C:\owned\packages" --packages .nuget-packages -p:UseAppHost=false
```

Copy the [standalone .NET example](../../fixtures/consumers/dotnet/Program.cs) to
`Program.cs`, then run `dotnet run --no-restore -p:UseAppHost=false`.
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
mise run sdk-tooling -- python distribution/verify_windows.py --architecture x64
```

This verifies a single DLL across C, .NET recovery and DPAPI fixtures, and the
isolated NuGet consumer. The Windows-only verification package and its receipt
are retained under `.native-build/win-x64/<version>/` and, when configured,
under `win-x64/<version>/` in `VM_RUNNER_ARTIFACTS_DIR`. The receipt records
the SDK and shared build helper hashes.

The hosted Windows distribution check runs the isolated consumer with Native AOT
and JSON reflection disabled before exporting the same DLL for the combined
package. The fixture exercises activation, persistence and reopen, refresh, and
deactivation using in-memory storage and temporary current-user DPAPI storage.
The combined package runs the same lifecycle fixture after restore.

On an owned x64 Windows worker with the Visual Studio C++ workload, append
`--native-aot` to the acceptance command to run the same check. On Windows ARM64,
run the standard command to verify x64 emulation. For local Native AOT checks,
the Windows acceptance receipt records success in `nativeAotFixtureExecuted`.

### macOS checks

`mise exec -- make test-dotnet` on macOS verifies the tracked
framework and runs managed lifecycle fixtures against it. It exercises fake
providers and in-memory storage. Windows x64 validation exercises DPAPI.
