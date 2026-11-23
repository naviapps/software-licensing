# Windows NuGet package

## Installation

`NaviApps.SoftwareLicensing` targets .NET 10 and supplies a `win-x64` native
asset. The SDK acceptance worker runs the x64 process under emulation on Windows
11 ARM64 (build 26100 or later). On that host select x64 .NET to run consumers.

A successful Windows job retains the tested package and
`software-licensing-windows.json`. Verify the package's SHA-256 against the
receipt under `win-x64/<version>/` in the private CI controller archive. Extract the `.nupkg` file from
that archive, leave the package itself intact, and use its directory as a local
NuGet source. The job does not upload a GitHub Actions artifact or publish to a
NuGet registry.

Set `DOTNET_BIN` to the absolute x64 .NET path. In a new consumer directory,
use a fresh package cache and replace the example source with the directory
containing the verified package:

```powershell
$env:NUGET_PACKAGES = Join-Path (Get-Location) ".nuget-packages"
& "$env:DOTNET_BIN" new console --framework net10.0
& "$env:DOTNET_BIN" add package NaviApps.SoftwareLicensing --version 0.1.0 --source C:\owned\packages
```

To verify native loading, replace `Program.cs` with the following and run
`& "$env:DOTNET_BIN" run --runtime win-x64 -p:UseAppHost=false`.

```csharp
using NaviApps.SoftwareLicensing;

Console.WriteLine(LicenseRefreshPolicy.Default.ValidationInterval);
```

With a package built from a revision containing `CallbackLicenseProvider`,
use this program to exercise a synthetic access check.

```csharp
using NaviApps.SoftwareLicensing;

var configuration = new CallbackLicenseProvider.Configuration("com.example.access", "membership");
var provider = new CallbackLicenseProvider(configuration, _ => Task.FromResult(true));
var activation = await provider.ActivateAsync(LicenseActivationRequest.Automatic);
Console.WriteLine(activation.PlanIdentifier);
```

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
[maintainer setup](https://github.com/naviapps/software-licensing#build-and-acceptance)
with a clean committed checkout on an owned Windows 11 ARM64 worker. Prepare
native ARM64 Python and the pinned ARM64 Rust toolchain with the
`x86_64-pc-windows-msvc` standard library. Select `HostARM64\x64` with
`VsDevCmd.bat -host_arch=arm64 -arch=x64`, set `DOTNET_BIN` to the absolute
path of the pinned x64 .NET SDK, and supply the approved full commit SHA of
the local SDK Tooling candidate.

```powershell
python -E -S -B tools/windows.py --architecture x64 `
  --tooling-source C:\owned\sdk-tooling --tooling-revision $approvedRevision
```

This verifies a single DLL across C, .NET recovery and DPAPI fixtures, and the
isolated NuGet consumer. The package and receipt are retained together under
`.native-build/win-x64/<version>/`. Point local package restore at that directory.
The GitHub-hosted job runs native x64 when the pinned tooling is publicly
accessible. On Windows ARM64, run the local command to verify x64 emulation.

### macOS checks

`mise exec -- make test-dotnet` on macOS verifies the tracked framework and
runs managed lifecycle fixtures against it. It exercises fake providers and
in-memory storage. The Windows x64 job exercises DPAPI.
