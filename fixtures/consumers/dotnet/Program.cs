using NaviApps.SoftwareLicensing;

#if NATIVE_AOT_FIXTURE
if (System.Runtime.CompilerServices.RuntimeFeature.IsDynamicCodeSupported ||
    System.Text.Json.JsonSerializer.IsReflectionEnabledByDefault)
    throw new InvalidOperationException("The fixture requires Native AOT with JSON reflection disabled.");
#endif

static void Require(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}

static async Task VerifyLifecycle(ILicenseStorage storage)
{
    var configuration = new CallbackLicenseProvider.Configuration("com.example.standalone", "paid");
    static DateTimeOffset Clock() => DateTimeOffset.FromUnixTimeSeconds(1_000);
    var provider = new CallbackLicenseProvider(configuration, _ => Task.FromResult(true), Clock);
    using (var manager = new LicenseManager(provider, storage, clock: Clock))
    {
        Require(manager.InitialRestoreError is null && !manager.State.Plan.IsLicensed, "Expected an empty license record.");
        var activated = await manager.ActivateAsync(LicenseActivationRequest.Automatic);
        Require(activated.Plan.IsLicensed && activated.Plan.Identifier == "paid", "Activation did not grant the expected plan.");
    }

    using var reopened = new LicenseManager(provider, storage, clock: Clock);
    Require(reopened.InitialRestoreError is null && reopened.State.Plan.IsLicensed && reopened.State.Source == configuration.Source,
        "Reopening lost the saved license.");
    var refreshed = await reopened.RefreshAsync();
    Require(refreshed.Outcome == LicenseRefreshOutcome.Validated && refreshed.State.Plan.IsLicensed, "Refresh did not retain access.");
    var released = await reopened.DeactivateAsync();
    Require(!released.Plan.IsLicensed && !released.HasActivation, "Deactivation did not clear access.");
}

await VerifyLifecycle(new MemoryStorage());
#if NATIVE_AOT_FIXTURE
if (OperatingSystem.IsWindowsVersionAtLeast(10, 0, 26100))
{
    var directory = Directory.CreateTempSubdirectory("software-licensing-aot-");
    try
    {
        await VerifyLifecycle(new WindowsLicenseStorage(directory.FullName, "com.example.standalone"));
    }
    finally { directory.Delete(recursive: true); }
}
#endif
Console.WriteLine("SoftwareLicensing external NuGet consumer passed.");

sealed class MemoryStorage : ILicenseStorage
{
    private byte[]? record;

    public byte[]? Load() => record?.ToArray();
    public void Save(byte[] value) => record = value.ToArray();
}
