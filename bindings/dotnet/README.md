# SoftwareLicensing for .NET

## Installation

`NaviApps.SoftwareLicensing` targets .NET 10 and supplies native assets for
macOS 26 or later on ARM64 or Intel, Windows x64, and Windows 11 ARM64 through x64
emulation (build 26100 or later). Choose the .NET SDK that matches your macOS
architecture, or the x64 .NET SDK on Windows. Check the architecture with
`dotnet --info`.

Download the combined NuGet artifact from an exact-revision
[distribution check](https://github.com/naviapps/software-licensing#distribution-checks).
Verify the package's SHA-256 against the accompanying `.nupkg.json` receipt.

In an empty project directory, replace the source path with the absolute path
to the directory containing the verified package. Use its exact version.

```sh
dotnet new console --framework net10.0 --no-restore
dotnet package add NaviApps.SoftwareLicensing --version "[0.1.0]" --no-restore
dotnet restore --source "/path/to/packages" --packages .nuget-packages -p:UseAppHost=false
```

## First use

Replace `Program.cs` with this example. It uses synthetic access and in-memory
storage.

```csharp
using NaviApps.SoftwareLicensing;

var configuration = new CallbackLicenseProvider.Configuration("com.example.app", "pro");
var provider = new CallbackLicenseProvider(configuration, _ => Task.FromResult(true));
using var manager = new LicenseManager(provider, new MemoryStorage());
if (manager.InitialRestoreError is { } failure)
    throw new LicenseException(failure);

var state = await manager.ActivateAsync(LicenseActivationRequest.Automatic);
Console.WriteLine(state.Plan.Identifier);

sealed class MemoryStorage : ILicenseStorage
{
    private byte[]? record;

    public byte[]? Load() => record?.ToArray();
    public void Save(byte[] value) => record = value.ToArray();
}
```

Run `dotnet run --no-restore -p:UseAppHost=false`. It prints `pro`.
The [standalone consumer fixture](https://github.com/naviapps/software-licensing/blob/main/fixtures/consumers/dotnet/Program.cs)
also tests reopen, refresh and deactivation.

## Application integration

For application integration, use `CallbackLicenseProvider` with a read-only
access check or supply an `ILicenseProvider` for key-based licensing. Implement
`ILicenseStorage` with atomic load and save for durable access. On Windows 11
build 26100 or later, `WindowsLicenseStorage` uses current-user DPAPI. Give each
record its own private absolute directory and keep its namespace stable.

Inspect `InitialRestoreError` before mutations. Keep the existing bytes and
call `Reload()` after correcting the storage or clock issue.
Read `LicenseException.Failure` for typed operation errors. The read-only
callback marks failures `NotApplied`, enabling a retry after the manager
clears the activation intent. If a remote activation's outcome is unknown,
resolve the pending intent with authoritative provider evidence before retrying.

Use `LicenseSource.NormalizeIdentifier`, `LicenseActivation.CreateValidated`,
`LicenseValidationResult.CreateValidated` and `LicenseRefreshPolicy.CreateValidated`
to check or normalize values before lifecycle operations. These calls use the
installed native library and throw `LicenseException` for invalid values.
The public record constructors still store the supplied values, and the
manager validates those values when it processes them.

`Plan` projects the current state. `NeedsRefresh()` uses the manager's clock,
and `NeedsRefresh(now)` uses the supplied observed time for refresh eligibility
and rollback protection. The query makes no provider or storage calls.
See [storage and recovery](https://github.com/naviapps/software-licensing#storage-and-recovery)
for the full persistence and provider recovery contract.
