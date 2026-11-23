using NaviApps.SoftwareLicensing;

static void Require(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}

static async Task<LicenseState> NextState(IAsyncEnumerator<LicenseState> observer)
{
    Require(await observer.MoveNextAsync(), "Expected a license snapshot.");
    return observer.Current;
}

var now = DateTimeOffset.FromUnixTimeSeconds(1_000);
var normalizedSource = LicenseSource.NormalizeIdentifier(" com.example.standalone ");
var validated = LicenseActivation.CreateValidated(normalizedSource, " paid ", now);
Require(validated.PlanIdentifier == "paid" && validated.Source == "com.example.standalone",
    "The public activation factory did not normalize the provider identity.");
Require(LicenseValidationResult.CreateValidated(true, " paid ").PlanIdentifier == "paid",
    "The public validation factory did not normalize its optional plan.");
Require(LicenseRefreshPolicy.CreateValidated(3600, 600, 300).FailureRetryInterval == 60,
    "The public refresh-policy factory lost its default retry interval.");
int clockReads = 0;
int accessChecks = 0;
bool clockThrows = false;
DateTimeOffset ReadClock()
{
    clockReads++;
    if (clockThrows) throw new InvalidOperationException("Fixture clock unavailable.");
    return now;
}

var configuration = new CallbackLicenseProvider.Configuration("com.example.standalone", "paid");
var provider = new CallbackLicenseProvider(configuration, _ =>
{
    accessChecks++;
    return Task.FromResult(true);
}, ReadClock);
var storage = new InMemoryLicenseStorage();
using (var manager = new LicenseManager(provider, storage, clock: ReadClock))
{
    Require(manager.InitialRestoreError is null && !manager.State.Plan.IsLicensed, "Expected an empty license record.");
    var activated = await manager.ActivateAsync(LicenseActivationRequest.Automatic);
    Require(activated.Plan.IsLicensed && activated.Status == LicenseStatus.Active,
        "The callback activation did not grant access.");
    Require(activated.Source == configuration.Source && activated.Plan.Identifier == "paid",
        "The callback activation lost its source or plan.");
    Require(storage.Load() is not null, "Activation did not save a record.");
}

using (var reopened = new LicenseManager(provider, storage, clock: ReadClock))
{
    var restored = reopened.State;
    Require(reopened.InitialRestoreError is null && restored.Plan.IsLicensed && restored.Plan.Identifier == "paid"
        && restored.Status == LicenseStatus.Active
        && restored.Source == configuration.Source && reopened.Plan == restored.Plan,
        "Reopening lost the saved license.");
    var deadline = restored.NextRefreshAt ?? throw new InvalidOperationException("Expected a refresh deadline.");
    Require(!reopened.NeedsRefresh(), "The host clock is before the refresh deadline.");
    var beforeQueries = (clockReads, accessChecks, storage.LoadCount, storage.SaveCount);
    Require(!reopened.NeedsRefresh(deadline.AddMilliseconds(-250)) && reopened.NeedsRefresh(deadline),
        "The explicit refresh query lost the inclusive deadline.");
    Require(reopened.NeedsRefresh(DateTimeOffset.MaxValue), "An unsupported explicit time must remain due.");
    Require(beforeQueries == (clockReads, accessChecks, storage.LoadCount, storage.SaveCount),
        "The explicit refresh query called the host clock, provider or storage.");
    now = deadline;
    var valid = reopened.State;
    var beforeClockFault = (accessChecks, storage.LoadCount, storage.SaveCount);
    using var observationDeadline = new CancellationTokenSource(TimeSpan.FromSeconds(5));
    await using var existingObserver = reopened.WatchStatesAsync(observationDeadline.Token).GetAsyncEnumerator();
    Require(await NextState(existingObserver) == valid, "Observer did not start with the saved license.");
    foreach (var throws in new[] { false, true })
    {
        now = throws ? deadline : DateTimeOffset.MaxValue;
        clockThrows = throws;
        var unavailable = reopened.State;
        Require(!unavailable.Plan.IsLicensed && unavailable.Status == LicenseStatus.Unlicensed
            && reopened.Plan == unavailable.Plan && reopened.NeedsRefresh(),
            "License queries did not fail closed while the clock was unavailable.");
        Require(await NextState(existingObserver) == unavailable, "Existing observer kept a stale grant.");
        await using var unavailableObserver = reopened.WatchStatesAsync(observationDeadline.Token).GetAsyncEnumerator();
        Require(await NextState(unavailableObserver) == unavailable, "New observer did not start unlicensed.");
        clockThrows = false;
        now = deadline;
        Require(reopened.State == valid, "Restoring the clock did not recover the saved license.");
        Require(await NextState(existingObserver) == valid && await NextState(unavailableObserver) == valid,
            "Observers did not recover the unchanged grant after the clock recovered.");
    }
    Require(beforeClockFault == (accessChecks, storage.LoadCount, storage.SaveCount),
        "Clock failure or recovery triggered provider or storage work.");
    var refreshed = await reopened.RefreshAsync();
    Require(refreshed.Outcome == LicenseRefreshOutcome.Validated && refreshed.State.Plan.IsLicensed,
        "The callback refresh did not retain access.");
    var deactivated = await reopened.DeactivateAsync();
    Require(!deactivated.Plan.IsLicensed && !deactivated.HasActivation,
        "The callback deactivation did not clear access.");
}

using var released = new LicenseManager(provider, storage, clock: ReadClock);
Require(released.InitialRestoreError is null && !released.State.Plan.IsLicensed && !released.State.HasActivation,
    "Reopening revived a cleared activation.");

var denied = new CallbackLicenseProvider(configuration, _ => Task.FromResult(false), ReadClock);
using var deniedManager = new LicenseManager(denied, new InMemoryLicenseStorage(), clock: ReadClock);
try
{
    await deniedManager.ActivateAsync(LicenseActivationRequest.Automatic);
    throw new InvalidOperationException("Denied callback activation must fail.");
}
catch (LicenseException error)
{
    Require(error.Failure.Code == LicenseErrorCode.InvalidLicense
        && error.Failure.MutationOutcome == LicenseMutationOutcome.NotApplied,
        "Denied callback activation lost its typed failure.");
}
Require(!deniedManager.State.Plan.IsLicensed && !deniedManager.State.HasPendingActivation,
    "Denied callback activation retained access or pending state.");

async Task<LicenseState> IdentitySnapshot(string source, string plan)
{
    var identity = new CallbackLicenseProvider.Configuration(source, plan);
    var callback = new CallbackLicenseProvider(identity, _ => Task.FromResult(true), ReadClock);
    using var manager = new LicenseManager(callback, new InMemoryLicenseStorage(), clock: ReadClock);
    return await manager.ActivateAsync(LicenseActivationRequest.Automatic);
}

const string composed = "caf\u00e9";
const string decomposed = "cafe\u0301";
var original = await IdentitySnapshot(composed, composed);
var sourceChange = await IdentitySnapshot(decomposed, composed);
var planChange = await IdentitySnapshot(composed, decomposed);
Require(original.Source != sourceChange.Source && original.Plan == sourceChange.Plan,
    "The snapshot merged distinct provider source spellings.");
Require(original.Source == planChange.Source && original.Plan != planChange.Plan,
    "The snapshot merged distinct plan identifier spellings.");
Require(new HashSet<LicenseState> { original, sourceChange, planChange }.Count == 3,
    "Snapshot equality merged distinct Rust identifiers.");
Console.WriteLine("SoftwareLicensing external NuGet consumer passed.");

sealed class InMemoryLicenseStorage : ILicenseStorage
{
    private byte[]? record;
    public int LoadCount { get; private set; }
    public int SaveCount { get; private set; }

    public byte[]? Load()
    {
        LoadCount++;
        return record?.ToArray();
    }

    public void Save(byte[] value)
    {
        SaveCount++;
        record = value.ToArray();
    }
}
