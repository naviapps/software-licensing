using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using NaviApps.SoftwareLicensing;

static void Check(bool value) { if (!value) throw new System.Diagnostics.UnreachableException("Assertion failed."); }
static async Task ExpectCancellation(Task operation)
{
    try { await operation.WaitAsync(TimeSpan.FromSeconds(5)); }
    catch (OperationCanceledException) { Check(operation.IsCanceled); return; }
    throw new System.Diagnostics.UnreachableException("Expected cancellation.");
}
static async Task ExpectStorageFailure(Task operation)
{
    try { await operation.WaitAsync(TimeSpan.FromSeconds(5)); }
    catch (LicenseException e) { Check(e.Failure.Code == LicenseErrorCode.StorageFailure); return; }
    throw new System.Diagnostics.UnreachableException("Expected storage failure.");
}
static async Task ExpectFailure(Task operation, LicenseFailure expected)
{
    try { await operation.WaitAsync(TimeSpan.FromSeconds(5)); }
    catch (LicenseException error) { Check(error.Failure == expected); return; }
    throw new System.Diagnostics.UnreachableException("Expected license failure.");
}
static void ExpectCode(Action operation, LicenseErrorCode expected)
{
    try { operation(); }
    catch (LicenseException error) { Check(error.Failure == new LicenseFailure(expected)); return; }
    throw new System.Diagnostics.UnreachableException("Expected license failure.");
}
static void MutateSyntheticSavedPlan(byte[] record)
{
    ReadOnlySpan<byte> before = "\"fixture-one\""u8;
    ReadOnlySpan<byte> after = "\"fixture-two\""u8;
    var remaining = record.AsSpan();
    int changes = 0;
    while (remaining.IndexOf(before) is var index && index >= 0)
    {
        after.CopyTo(remaining[index..]);
        remaining = remaining[(index + before.Length)..];
        changes++;
    }
    Check(changes > 0);
}
static async Task CheckCancelledAdmission(LicenseManager manager, MemoryStorage storage, ReleaseProvider provider)
{
    using var cancellation = new CancellationTokenSource();
    cancellation.Cancel();
    var state = manager.State;
    var record = storage.Data;
    var restoreError = manager.InitialRestoreError;
    var saves = storage.SaveCount;
    var loads = storage.LoadCount;
    var calls = (provider.ActivationCount, provider.ValidationCount, provider.ReleaseCount);
    Func<Task>[] operations =
    [
        () => manager.ActivateAsync(new("ANOTHER-KEY"), cancellation.Token),
        () => manager.RefreshAsync(cancellationToken: cancellation.Token),
        () => manager.DeactivateAsync(cancellation.Token),
        () => manager.RetryPersistenceAsync(cancellation.Token),
    ];
    foreach (var invoke in operations)
    {
        await ExpectCancellation(invoke());
        Check(manager.State == state && manager.InitialRestoreError == restoreError);
        Check(ReferenceEquals(storage.Data, record) && storage.SaveCount == saves && storage.LoadCount == loads);
        Check((provider.ActivationCount, provider.ValidationCount, provider.ReleaseCount) == calls);
    }
}
string[] outcomeNames = ["validated", "gracePeriod", "expired", "invalid", "deactivated",
    "skippedActivationInProgress", "skippedRefreshInProgress", "skippedDeactivationInProgress",
    "skippedNoActivation", "skippedRefreshDisabled", "skippedNotDue"];
var outcomes = Enum.GetValues<LicenseRefreshOutcome>();
Check(!JsonSerializer.IsReflectionEnabledByDefault);
Check(outcomes.Length == outcomeNames.Length);
for (int i = 0; i < outcomes.Length; i++)
{
    var encoded = JsonSerializer.Serialize(outcomes[i], LicensingJson.Type<LicenseRefreshOutcome>());
    Check(encoded == JsonSerializer.Serialize(outcomeNames[i], LicensingJson.Type<string>()));
    Check(JsonSerializer.Deserialize(encoded, LicensingJson.Type<LicenseRefreshOutcome>()) == outcomes[i]);
}
foreach (var invalid in new[] { "\"futureOutcome\"", "\"refreshed\"", "0", "\"0\"" })
{
    try { JsonSerializer.Deserialize(invalid, LicensingJson.Type<LicenseRefreshOutcome>()); throw new System.Diagnostics.UnreachableException("Unknown outcome accepted."); }
    catch (JsonException) { }
}
Native.RequireAbi(6);
foreach (var oldAbi in new uint[] { 1, 2, 3, 4, 5 })
{
    try { Native.RequireAbi(oldAbi); throw new System.Diagnostics.UnreachableException("Old ABI accepted."); }
    catch (LicenseException error) { Check(error.Failure.Code == LicenseErrorCode.IncompatibleAbiVersion); }
}
var failureType = LicensingJson.Type<LicenseFailure>();
Check(JsonSerializer.Deserialize("""{"code":"unsupportedVersion"}""", failureType)?.Code == LicenseErrorCode.UnsupportedVersion);
Check(JsonSerializer.Serialize(new LicenseFailure(LicenseErrorCode.IncompatibleAbiVersion), failureType) == """{"code":"incompatibleAbiVersion"}""");
var fullFailure = new LicenseFailure(LicenseErrorCode.Cancelled, 429, ulong.MaxValue, LicenseMutationOutcome.NotApplied);
var fullFailureJson = JsonSerializer.Serialize(fullFailure, failureType);
Check(JsonSerializer.Deserialize(fullFailureJson, failureType) == fullFailure);
using (var wire = JsonDocument.Parse(fullFailureJson))
{
    Check(wire.RootElement.GetProperty("mutationOutcome").GetString() == "notApplied");
    Check(wire.RootElement.GetProperty("retryAfterSeconds").GetUInt64() == ulong.MaxValue);
}
Check(JsonSerializer.Deserialize("""{"code":"mutationOutcomeUnknown","mutationOutcome":"unknown"}""", failureType)
    == new LicenseFailure(LicenseErrorCode.MutationOutcomeUnknown, MutationOutcome: LicenseMutationOutcome.Unknown));
Check(JsonSerializer.Deserialize("""{"code":"activationPending"}""", failureType)
    == new LicenseFailure(LicenseErrorCode.ActivationPending));
try { JsonSerializer.Deserialize("\"applied\"", LicensingJson.Type<LicenseMutationOutcome>()); throw new System.Diagnostics.UnreachableException("Unknown mutation accepted."); }
catch (JsonException) { }
if (args.Contains("--managed-only"))
{
    Console.WriteLine("Managed refresh vocabulary, unknown-value rejection, ABI distinction and legacy failure decoding passed; no native library loaded.");
    return;
}
var clock = () => DateTimeOffset.FromUnixTimeSeconds(2000);
{
    const string literalReplacement = "test-\ufffd";
    var journal = new MemoryStorage();
    var provider = new ReleaseProvider
    {
        ActivationResponse = () => new LicenseActivation("test", "paid", clock(), literalReplacement, "instance"),
    };
    using var manager = new LicenseManager(provider, journal, clock: clock);
    var initialState = manager.State;
    var initialLoads = journal.LoadCount;
    foreach (var key in new[] { "test-\ud800", "test-\udc00" })
    {
        await ExpectFailure(manager.ActivateAsync(new(key)), new LicenseFailure(LicenseErrorCode.InvalidData));
        Check(manager.State == initialState && journal.LoadCount == initialLoads && journal.SaveCount == 0);
        Check(provider.ActivationCount == 0);
    }

    var active = await manager.ActivateAsync(new(literalReplacement));
    Check(active.Plan.IsLicensed && provider.ActivationCount == 1 && journal.SaveCount == 2);
    Check(provider.LastActivationRequest?.LicenseKey == literalReplacement);
}
{
    const string malformed = "fixture-\ud800";
    const string literalReplacement = "fixture-\ufffd";
    var valid = new LicenseActivation("test", "paid", clock(), "TEST-KEY", literalReplacement);
    foreach (var reply in new[]
    {
        valid with { ActivationIdentifier = malformed },
        valid with { Source = malformed },
        valid with { PlanIdentifier = malformed },
        valid with { LicenseKey = malformed },
    })
    {
        var journal = new MemoryStorage();
        var provider = new ReleaseProvider { ActivationResponse = () => reply };
        using (var manager = new LicenseManager(provider, journal, clock: clock))
        {
            await ExpectFailure(manager.ActivateAsync(new("TEST-KEY")), new(LicenseErrorCode.InvalidData));
            Check(!manager.State.IsActivating && !manager.State.HasPendingPersistence &&
                manager.State.HasPendingActivation && !manager.State.Plan.IsLicensed);
            Check(provider.ActivationCount == 1 && journal.SaveCount == 1);
        }
        using var reopened = new LicenseManager(provider, journal, clock: clock);
        Check(reopened.InitialRestoreError is null && reopened.State.HasPendingActivation && !reopened.State.HasActivation);
    }

    var verifiedStorage = new MemoryStorage();
    var unusedProvider = new ReleaseProvider();
    using (var manager = new LicenseManager(unusedProvider, verifiedStorage, clock: clock))
    {
        var initialState = manager.State;
        ExpectCode(() => manager.AdoptExternallyVerifiedActivation(valid with { ActivationIdentifier = malformed }),
            LicenseErrorCode.InvalidData);
        Check(manager.State == initialState && verifiedStorage.SaveCount == 0);
        Check(manager.AdoptExternallyVerifiedActivation(valid).Plan.IsLicensed && unusedProvider.ActivationCount == 0);
        using var saved = JsonDocument.Parse(verifiedStorage.Data!);
        Check(saved.RootElement.GetProperty("activation").GetProperty("activationIdentifier").GetString() == literalReplacement);
    }

    var unreadable = new MemoryStorage { Data = "unreadable"u8.ToArray() };
    using var blocked = new LicenseManager(unusedProvider, unreadable, clock: clock);
    ExpectCode(() => blocked.AdoptExternallyVerifiedActivation(valid with { ActivationIdentifier = malformed }),
        LicenseErrorCode.StorageFailure);
    Check(unreadable.SaveCount == 0 && unreadable.Data.AsSpan().SequenceEqual("unreadable"u8));
}
{
    var journal = new MemoryStorage();
    var provider = new ReleaseProvider { ValidationResult = new(true, "plan-\udc00") };
    using var manager = new LicenseManager(provider, journal, clock: clock);
    var active = await manager.ActivateAsync(new("TEST-KEY"));
    var persisted = journal.Data;
    await ExpectFailure(manager.RefreshAsync(), new(LicenseErrorCode.InvalidData));
    Check(manager.State == active && provider.ValidationCount == 1 && journal.SaveCount == 2 &&
        ReferenceEquals(journal.Data, persisted));

    const string literalReplacement = "plan-\ufffd";
    provider.ValidationResult = new(true, literalReplacement);
    Check((await manager.RefreshAsync()).State.Plan.Identifier == literalReplacement && journal.SaveCount == 3);
}
{
    const string literalReplacement = "fixture-\ufffd";
    var accessChecks = 0;
    var provider = new CallbackLicenseProvider(new(literalReplacement, "paid"), _ =>
    {
        accessChecks++;
        return Task.FromResult(true);
    }, clock);
    var activation = new LicenseActivation(literalReplacement, "paid", clock());
    await ExpectFailure(provider.ValidateAsync(activation with { Source = "fixture-\ud800" }),
        new(LicenseErrorCode.InvalidData, MutationOutcome: LicenseMutationOutcome.NotApplied));
    Check(accessChecks == 0);
    Check((await provider.ValidateAsync(activation)).IsValid && accessChecks == 1);
}
{
    var journal = new MemoryStorage { FailOnSave = 1 };
    var activation = new LicenseActivation("test", "café", clock());
    using (var manager = new LicenseManager(new Provider(false), journal, clock: clock))
    {
        ExpectCode(() => manager.AdoptExternallyVerifiedActivation(activation), LicenseErrorCode.StorageFailure);
        await manager.RetryPersistenceAsync();
        Check(manager.State.Plan.Identifier == "café" && journal.SaveCount == 2);
        Check(journal.Data.AsSpan().IndexOf("café"u8) >= 0);
    }
    using var restored = new LicenseManager(new Provider(false), journal, clock: clock);
    Check(restored.InitialRestoreError is null && restored.Plan.Identifier == "café");
}
{
    var now = clock();
    Check(LicenseSource.NormalizeIdentifier(" fixture ") == "fixture");
    var raw = new LicenseActivation(" fixture ", " paid ", now, " A B ", " instance ", now.AddSeconds(1));
    Check(raw.Source == " fixture " && raw.LicenseKey == " A B ");
    Check(LicenseActivation.CreateValidated(raw.Source, raw.PlanIdentifier, raw.ActivatedAt,
        raw.LicenseKey, raw.ActivationIdentifier, raw.ExpiresAt) == new LicenseActivation("fixture", "paid", now, "AB", "instance", now.AddSeconds(1)));
    ExpectCode(() => LicenseActivation.CreateValidated("fixture", "unlicensed", now), LicenseErrorCode.InvalidData);

    Check(LicenseValidationResult.CreateValidated(true, " paid ") == new LicenseValidationResult(true, "paid"));
    ExpectCode(() => LicenseValidationResult.CreateValidated(false, "paid"), LicenseErrorCode.UnexpectedProviderResponse);

    Check(LicenseRefreshPolicy.CreateValidated(3600, 600, 300) == new LicenseRefreshPolicy(true, 3600, 600, 300));
    ExpectCode(() => LicenseRefreshPolicy.CreateValidated(double.NaN, 600, 300), LicenseErrorCode.InvalidData);

    const string unpairedSurrogate = "\ud800";
    ExpectCode(() => LicenseSource.NormalizeIdentifier(unpairedSurrogate), LicenseErrorCode.InvalidData);
    ExpectCode(() => LicenseActivation.CreateValidated("fixture", "paid", now, licenseKey: unpairedSurrogate), LicenseErrorCode.InvalidData);
    ExpectCode(() => LicenseValidationResult.CreateValidated(true, unpairedSurrogate), LicenseErrorCode.InvalidData);
}
{
    ExpectCode(() => _ = new CallbackLicenseProvider.Configuration("host-membership", "unlicensed"),
        LicenseErrorCode.InvalidProviderConfiguration);
    ExpectCode(() => _ = new CallbackLicenseProvider.Configuration("", "paid"),
        LicenseErrorCode.InvalidProviderConfiguration);

    var configuration = new CallbackLicenseProvider.Configuration(" host-membership ", " paid ");
    Check(configuration.Source == "host-membership" && configuration.PlanIdentifier == "paid");
    var checks = 0;
    var allowed = true;
    var time = clock();
    var callback = new CallbackLicenseProvider(configuration, _ =>
    {
        checks++;
        time = time.AddSeconds(1);
        return Task.FromResult(allowed);
    }, () => time);
    var activation = new LicenseActivation("host-membership", "paid", clock().AddSeconds(1));
    Check(await callback.ActivateAsync(LicenseActivationRequest.Automatic) == activation && checks == 1);
    Check((await callback.ValidateAsync(activation)).IsValid && checks == 2);
    await callback.DeactivateAsync(activation);
    await ExpectFailure(callback.ActivateAsync(new("TEST-KEY")),
        new(LicenseErrorCode.InvalidLicenseKey, MutationOutcome: LicenseMutationOutcome.NotApplied));
    var mismatched = activation with { ActivationIdentifier = "instance" };
    Check(!(await callback.ValidateAsync(mismatched)).IsValid);
    await ExpectFailure(callback.DeactivateAsync(mismatched),
        new(LicenseErrorCode.InvalidLicense, MutationOutcome: LicenseMutationOutcome.NotApplied));
    Check(checks == 2);
    allowed = false;
    await ExpectFailure(callback.ActivateAsync(LicenseActivationRequest.Automatic),
        new(LicenseErrorCode.InvalidLicense, MutationOutcome: LicenseMutationOutcome.NotApplied));
    using var canceled = new CancellationTokenSource();
    canceled.Cancel();
    await ExpectFailure(callback.ActivateAsync(LicenseActivationRequest.Automatic, canceled.Token),
        new(LicenseErrorCode.Cancelled, MutationOutcome: LicenseMutationOutcome.NotApplied));
    Check(checks == 3);
}
foreach (var original in new Exception[]
{
    new IOException("Synthetic failure"),
    new LicenseException(new LicenseFailure(LicenseErrorCode.RateLimited, 429, 60, LicenseMutationOutcome.Unknown)),
    new OperationCanceledException(),
})
{
    var expected = original switch
    {
        LicenseException error => error.Failure with { MutationOutcome = LicenseMutationOutcome.NotApplied },
        OperationCanceledException => new LicenseFailure(LicenseErrorCode.Cancelled, MutationOutcome: LicenseMutationOutcome.NotApplied),
        _ => new LicenseFailure(LicenseErrorCode.TransportFailure, MutationOutcome: LicenseMutationOutcome.NotApplied),
    };
    var checks = 0;
    var config = new CallbackLicenseProvider.Configuration("host-membership", "paid");
    var callback = new CallbackLicenseProvider(config, _ =>
        ++checks == 1 ? Task.FromException<bool>(original) : Task.FromResult(true), clock);
    var journal = new MemoryStorage();
    using (var manager = new LicenseManager(callback, journal, clock: clock))
    {
        var result = manager.ActivateAsync(LicenseActivationRequest.Automatic);
        if (expected.Code == LicenseErrorCode.Cancelled)
            await ExpectCancellation(result);
        else
            await ExpectFailure(result, expected);
        Check(checks == 1 && journal.SaveCount == 2);
        Check(!manager.State.HasPendingActivation && !manager.State.HasPendingPersistence && !manager.State.Plan.IsLicensed);
        Check(manager.State.LastOperationFailure == expected);
    }
    using var restored = new LicenseManager(callback, journal, clock: clock);
    Check(restored.InitialRestoreError is null && restored.State.LastOperationFailure == expected);
    Check((await restored.ActivateAsync(LicenseActivationRequest.Automatic)).Plan.IsLicensed);
    Check((await restored.RefreshAsync()).Outcome == LicenseRefreshOutcome.Validated);
    Check(!(await restored.DeactivateAsync()).HasActivation && checks == 3);
}
foreach (var returnsLateAccess in new[] { false, true })
{
    using var cancellation = new CancellationTokenSource();
    var callback = new CallbackLicenseProvider(new("host-membership", "paid"), token =>
    {
        Check(token == cancellation.Token);
        cancellation.Cancel();
        return returnsLateAccess ? Task.FromResult(true) : Task.FromException<bool>(new IOException("Synthetic failure"));
    }, clock);
    var journal = new MemoryStorage();
    using var manager = new LicenseManager(callback, journal, clock: clock);
    await ExpectCancellation(manager.ActivateAsync(LicenseActivationRequest.Automatic, cancellation.Token));
    Check(!manager.State.Plan.IsLicensed && !manager.State.HasPendingActivation && journal.SaveCount == 2);
    Check(manager.State.LastOperationFailure ==
        new LicenseFailure(LicenseErrorCode.Cancelled, MutationOutcome: LicenseMutationOutcome.NotApplied));
}
var storage = new MemoryStorage();
using (var manager = new LicenseManager(new Provider(true), storage, clock: clock))
{
    storage.Fails = true;
    try { await manager.ActivateAsync(new("TEST-KEY")); throw new System.Diagnostics.UnreachableException("Expected storage failure"); }
    catch (LicenseException e) { Check(e.Failure.Code == LicenseErrorCode.StorageFailure); }
    Check(manager.State.HasPendingPersistence && !manager.State.Plan.IsLicensed);
    storage.Fails = false;
    await manager.RetryPersistenceAsync();
    Check(manager.State.Plan.IsLicensed);
    var refresh = await manager.RefreshAsync();
    Check(refresh.Outcome == LicenseRefreshOutcome.GracePeriod && refresh.State.Status == LicenseStatus.GracePeriod);
    try { await manager.DeactivateAsync(); throw new System.Diagnostics.UnreachableException("Expected unknown release"); }
    catch (LicenseException e) { Check(e.Failure.Code == LicenseErrorCode.MutationOutcomeUnknown); }
    Check(manager.State.LastOperationFailure?.Code == LicenseErrorCode.TransportFailure);
    Check(manager.State.HasPendingDeactivation && !manager.State.Plan.IsLicensed);
    Check(!JsonSerializer.Serialize(manager.State, LicensingJson.Type<LicenseState>()).Contains("TEST-KEY"));
}
using (var restored = new LicenseManager(new Provider(false), storage, clock: clock))
{
    Check(restored.State.HasPendingDeactivation);
    var released = await restored.RefreshAsync();
    Check(released.Outcome == LicenseRefreshOutcome.Deactivated && !restored.State.HasActivation);
}
var releaseStorage = new MemoryStorage();
var releaseProvider = new ReleaseProvider();
using (var recovery = new LicenseManager(releaseProvider, releaseStorage, clock: clock))
{
    await recovery.ActivateAsync(new("TEST-KEY"));
    var activatedRecord = releaseStorage.Data;
    releaseStorage.Fails = true;
    try { await recovery.DeactivateAsync(); throw new System.Diagnostics.UnreachableException("Expected marker write failure"); }
    catch (LicenseException e) { Check(e.Failure.Code == LicenseErrorCode.StorageFailure); }
    Check(ReferenceEquals(releaseStorage.Data, activatedRecord) && releaseProvider.ReleaseCount == 0);
    Check(recovery.State.HasPendingPersistence && !recovery.State.Plan.IsLicensed);
    releaseStorage.Fails = false;
    var rejectedNestedRetry = false;
    releaseStorage.BeforeSave = () =>
    {
        // A second thread must be able to query state while a host storage callback runs.
        Check(Task.Run(() => recovery.State).Wait(TimeSpan.FromSeconds(5)));
        // Synchronous storage can re-enter the manager on the same thread. It
        // must not issue another write while this attempt awaits its receipt.
        try { recovery.RetryPersistenceAsync().GetAwaiter().GetResult(); }
        catch (LicenseException e)
        {
            Check(e.Failure.Code == LicenseErrorCode.OperationInProgress);
            rejectedNestedRetry = true;
        }
    };
    await recovery.RetryPersistenceAsync();
    Check(rejectedNestedRetry && releaseProvider.ReleaseCount == 1);
    Check(releaseStorage.SaveCount == 5);
    Check(!recovery.State.HasActivation && !recovery.State.HasPendingPersistence);
}
using (var restored = new LicenseManager(releaseProvider, releaseStorage, clock: clock))
    Check(restored.InitialRestoreError is null && !restored.State.HasActivation && !restored.State.Plan.IsLicensed);
// All four async entry points must leave pending activation, release-marker and
// final-release writes untouched when cancellation has already been requested.
foreach (var pendingWrite in new[] { "activation", "releaseMarker", "finalRelease" })
{
    var pendingStorage = new MemoryStorage();
    var pendingProvider = new ReleaseProvider();
    using var pending = new LicenseManager(pendingProvider, pendingStorage, clock: clock);
    if (pendingWrite != "activation") await pending.ActivateAsync(new("TEST-KEY")).WaitAsync(TimeSpan.FromSeconds(5));
    pendingStorage.FailOnSave = pendingStorage.SaveCount + (pendingWrite == "finalRelease" ? 2 : 1);
    await ExpectStorageFailure(pendingWrite == "activation"
        ? pending.ActivateAsync(new("TEST-KEY")) : pending.DeactivateAsync());
    Check(pending.State.HasPendingPersistence && !pending.State.Plan.IsLicensed);
    Check(pendingProvider.ActivationCount == (pendingWrite == "activation" ? 0 : 1));
    try { pending.Dispose(); throw new System.Diagnostics.UnreachableException("Pending persistence was discarded."); }
    catch (LicenseException error) { Check(error.Failure.Code == LicenseErrorCode.PersistencePending); }
    pendingStorage.FailOnSave = null;
    await CheckCancelledAdmission(pending, pendingStorage, pendingProvider);
    await pending.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
    await pending.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
    Check(!pending.State.HasPendingPersistence && pendingProvider.ActivationCount == 1);
    Check(pendingProvider.ReleaseCount == (pendingWrite == "activation" ? 0 : 1));
    Check(pending.State.HasActivation == (pendingWrite == "activation"));
}
var blockedStorage = new MemoryStorage { Data = Encoding.UTF8.GetBytes("""{"version":0}""") };
var blockedProvider = new ReleaseProvider();
using (var blocked = new LicenseManager(blockedProvider, blockedStorage, clock: clock))
{
    Check(blocked.InitialRestoreError?.Code == LicenseErrorCode.UnsupportedVersion);
    blockedStorage.Data = null;
    await CheckCancelledAdmission(blocked, blockedStorage, blockedProvider);
    await blocked.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
    Check(blocked.InitialRestoreError is null && blockedStorage.LoadCount == 2);
}
{
    var liveStorage = new MemoryStorage();
    var liveProvider = new ReleaseProvider();
    using var live = new LicenseManager(liveProvider, liveStorage, clock: clock);
    Check(live.InitialRestoreError is null && liveStorage.LoadCount == 1 && liveStorage.SaveCount == 0);
    var active = live.AdoptExternallyVerifiedActivation(new("fixture", "paid", DateTimeOffset.FromUnixTimeSeconds(1000)));
    var validRecord = liveStorage.Data!;
    var liveSaves = liveStorage.SaveCount;
    foreach (var (json, expected) in new[]
    {
        ("", LicenseErrorCode.StorageFailure),
        (" \nnull\t", LicenseErrorCode.StorageFailure),
        ("true", LicenseErrorCode.StorageFailure),
        ("false", LicenseErrorCode.StorageFailure),
        ("42", LicenseErrorCode.StorageFailure),
        ("\"fixture\"", LicenseErrorCode.StorageFailure),
        ("{", LicenseErrorCode.StorageFailure),
        ("[]", LicenseErrorCode.UnsupportedVersion),
        ("{\"version\":2}", LicenseErrorCode.InvalidData),
    })
    {
        var invalid = Encoding.UTF8.GetBytes(json);
        var original = invalid.ToArray();
        var persisted = new MemoryStorage { Data = invalid };
        var persistedProvider = new ReleaseProvider();
        var forbidInitialClock = expected == LicenseErrorCode.StorageFailure;
        Func<DateTimeOffset> restoreClock = () => forbidInitialClock
            ? throw new InvalidOperationException("Malformed storage should be rejected before sampling time.")
            : clock();
        using (var eager = new LicenseManager(persistedProvider, persisted, clock: restoreClock))
        {
            forbidInitialClock = false;
            Check(eager.InitialRestoreError == new LicenseFailure(expected) && !eager.State.Plan.IsLicensed);
            await ExpectFailure(eager.ActivateAsync(new("FIXTURE-KEY")), new(expected));
            Check(persisted.LoadCount == 2);
        }
        using (var deferred = new LicenseManager(persistedProvider, persisted, restorePersistedActivation: false, clock: clock))
        {
            Check(deferred.InitialRestoreError is null && persisted.LoadCount == 2);
            ExpectCode(deferred.Reload, expected);
            Check(persisted.LoadCount == 3);
        }
        Check(ReferenceEquals(persisted.Data, invalid) && persisted.SaveCount == 0);
        Check((persistedProvider.ActivationCount, persistedProvider.ValidationCount, persistedProvider.ReleaseCount) == (0, 0, 0));

        liveStorage.Data = invalid;
        ExpectCode(live.Reload, expected);
        Check(live.State == active && live.InitialRestoreError is null);
        Check(ReferenceEquals(liveStorage.Data, invalid) && liveStorage.SaveCount == liveSaves);
        Check(invalid.AsSpan().SequenceEqual(original));
    }
    var recordJson = Encoding.UTF8.GetString(validRecord);
    var padding = new string(' ', 1024 * 1024);
    foreach (var bytes in new[]
    {
        validRecord,
        Encoding.UTF8.GetBytes(recordJson + padding),
        Encoding.UTF8.GetBytes("{" + padding + recordJson[1..]),
        Encoding.Unicode.GetPreamble().Concat(Encoding.Unicode.GetBytes(recordJson)).ToArray(),
    })
    {
        liveStorage.Data = bytes;
        live.Reload();
        Check(live.State == active && ReferenceEquals(liveStorage.Data, bytes));
    }
    liveStorage.Data = null;
    live.Reload();
    Check(live.InitialRestoreError is null && !live.State.HasActivation && liveStorage.Data is null);
    Check(liveStorage.SaveCount == liveSaves);
    Check((liveProvider.ActivationCount, liveProvider.ValidationCount, liveProvider.ReleaseCount) == (0, 0, 0));
}
// Cancellation during the marker save must acknowledge the provider effect as
// cancelled, even if the provider itself never checks the token.
foreach (var retryMarker in new[] { false, true })
{
    var cancelledStorage = new MemoryStorage();
    var cancelledProvider = new ReleaseProvider();
    using var cancelled = new LicenseManager(cancelledProvider, cancelledStorage, clock: clock);
    await cancelled.ActivateAsync(new("TEST-KEY")).WaitAsync(TimeSpan.FromSeconds(5));
    if (retryMarker)
    {
        cancelledStorage.Fails = true;
        await ExpectStorageFailure(cancelled.DeactivateAsync());
        cancelledStorage.Fails = false;
    }
    using var cancellation = new CancellationTokenSource();
    cancelledStorage.BeforeSave = cancellation.Cancel;
    var saves = cancelledStorage.SaveCount;
    await ExpectCancellation(retryMarker
        ? cancelled.RetryPersistenceAsync(cancellation.Token) : cancelled.DeactivateAsync(cancellation.Token));
    Check(cancellation.IsCancellationRequested && cancelledProvider.ReleaseCount == 0);
    Check(cancelledStorage.SaveCount == saves + 2);
    Check(cancelled.State.LastOperationFailure == new LicenseFailure(LicenseErrorCode.Cancelled,
        MutationOutcome: LicenseMutationOutcome.NotApplied));
    Check(cancelled.State.HasPendingDeactivation && cancelled.State.HasActivation);
    Check(!cancelled.State.IsDeactivating && !cancelled.State.HasPendingPersistence && !cancelled.State.Plan.IsLicensed);
    using (var marker = JsonDocument.Parse(cancelledStorage.Data!))
    {
        Check(marker.RootElement.GetProperty("pendingDeactivation").GetBoolean());
        Check(marker.RootElement.GetProperty("activation").GetProperty("activationIdentifier").GetString() == "instance");
    }
    await cancelled.RefreshAsync().WaitAsync(TimeSpan.FromSeconds(5));
    await cancelled.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
    Check(cancelledProvider.ReleaseCount == 1 && !cancelled.State.HasActivation);
}
// Once a noncooperative provider has entered and succeeded, cancellation cannot
// discard its actual activation or release, including when the final save fails.
foreach (var failSave in new[] { false, true })
{
    var completedStorage = new MemoryStorage { FailOnSave = failSave ? 2 : null };
    var completedProvider = new ReleaseProvider();
    using var completed = new LicenseManager(completedProvider, completedStorage, clock: clock);
    using var activationCancellation = new CancellationTokenSource();
    completedProvider.BeforeActivate = activationCancellation.Cancel;
    var activation = completed.ActivateAsync(new("TEST-KEY"), activationCancellation.Token);
    if (failSave) await ExpectStorageFailure(activation);
    else Check((await activation.WaitAsync(TimeSpan.FromSeconds(5))).Plan.IsLicensed);
    Check(activationCancellation.IsCancellationRequested && completedProvider.ActivationCount == 1);
    Check(completedStorage.SaveCount == 2 && completed.State.HasPendingPersistence == failSave);
    completedStorage.FailOnSave = null;
    await completed.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
    await completed.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
    Check(completed.State.Plan.IsLicensed && !completed.State.HasPendingPersistence && completedProvider.ActivationCount == 1);
    using (var record = JsonDocument.Parse(completedStorage.Data!))
    {
        var savedActivation = record.RootElement.GetProperty("activation");
        Check(savedActivation.GetProperty("activationIdentifier").GetString() == "instance");
        Check(savedActivation.GetProperty("licenseKey").GetString() == "TEST-KEY");
    }
    using var releaseCancellation = new CancellationTokenSource();
    completedProvider.BeforeRelease = releaseCancellation.Cancel;
    var saves = completedStorage.SaveCount;
    if (failSave) completedStorage.FailOnSave = saves + 2;
    var release = completed.DeactivateAsync(releaseCancellation.Token);
    if (failSave) await ExpectStorageFailure(release);
    else Check(!(await release.WaitAsync(TimeSpan.FromSeconds(5))).HasActivation);
    Check(releaseCancellation.IsCancellationRequested && completedProvider.ReleaseCount == 1);
    Check(completedStorage.SaveCount == saves + 2 && completed.State.HasPendingPersistence == failSave);
    completedStorage.FailOnSave = null;
    await completed.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
    await completed.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
    Check(completedProvider.ActivationCount == 1 && completedProvider.ReleaseCount == 1);
    Check(!completed.State.HasActivation && !completed.State.HasPendingPersistence && !completed.State.HasPendingDeactivation);
    using (var record = JsonDocument.Parse(completedStorage.Data!))
    {
        Check(record.RootElement.GetProperty("activation").ValueKind == JsonValueKind.Null);
        Check(!record.RootElement.GetProperty("pendingDeactivation").GetBoolean());
    }
}
Console.WriteLine(".NET cancelled admission, provider dispatch and durable provider completion passed.");
storage.Data = Encoding.UTF8.GetBytes("""{"version":0}""");
var unsupportedBytes = storage.Data.ToArray();
using (var unsupported = new LicenseManager(new Provider(false), storage))
{
    Check(unsupported.InitialRestoreError?.Code == LicenseErrorCode.UnsupportedVersion);
    Check(!unsupported.State.Plan.IsLicensed && storage.Data.SequenceEqual(unsupportedBytes));
}
var paused = new PausedProvider();
var observed = new LicenseManager(paused, new MemoryStorage(), clock: clock);
observed.AdoptExternallyVerifiedActivation(new("test", "paid", DateTimeOffset.FromUnixTimeSeconds(1000),
    "PRIVATE-KEY", "instance", DateTimeOffset.FromUnixTimeSeconds(2100)));
await using (var states = observed.WatchStatesAsync().GetAsyncEnumerator())
{
    Check(await states.MoveNextAsync().AsTask().WaitAsync(TimeSpan.FromSeconds(5)));
    Check(states.Current.NextTransitionAt == DateTimeOffset.FromUnixTimeSeconds(2100));
    var refresh = observed.RefreshAsync();
    Check(await states.MoveNextAsync().AsTask().WaitAsync(TimeSpan.FromSeconds(5)) && states.Current.IsRefreshing);
    paused.Validation.SetException(new LicenseException(LicenseErrorCode.TransportFailure));
    await refresh;
    Check(await states.MoveNextAsync().AsTask().WaitAsync(TimeSpan.FromSeconds(5)));
    Check(!states.Current.IsRefreshing && states.Current.Status == LicenseStatus.GracePeriod);
    Check(states.Current.NextTransitionAt == DateTimeOffset.FromUnixTimeSeconds(2100));
    Check(!JsonSerializer.Serialize(states.Current, LicensingJson.Type<LicenseState>()).Contains("PRIVATE-KEY"));
    observed.Dispose();
    Check(!await states.MoveNextAsync().AsTask().WaitAsync(TimeSpan.FromSeconds(5)));
}
{
    var time = clock();
    int reads = 0;
    bool clockThrows = false;
    DateTimeOffset ReadClock()
    {
        reads++;
        if (clockThrows) throw new InvalidOperationException("Fixture clock unavailable.");
        return time;
    }

    var clockStorage = new MemoryStorage();
    var clockProvider = new ReleaseProvider();
    using var manager = new LicenseManager(clockProvider, clockStorage, clock: ReadClock);
    await manager.ActivateAsync(new("TEST-KEY"));
    var deadline = manager.State.NextRefreshAt!.Value;
    var beforeQueries = (reads, clockStorage.LoadCount, clockStorage.SaveCount);
    Check(!manager.NeedsRefresh(deadline.AddMilliseconds(-250)) && manager.NeedsRefresh(deadline));
    Check(manager.NeedsRefresh(DateTimeOffset.MaxValue));
    Check(beforeQueries == (reads, clockStorage.LoadCount, clockStorage.SaveCount));
    time = deadline;
    var available = manager.State;
    Check(available.Plan.IsLicensed);
    using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(5));
    await using var observer = manager.WatchStatesAsync(timeout.Token).GetAsyncEnumerator();
    Check(await observer.MoveNextAsync() && observer.Current == available);
    foreach (var throws in new[] { false, true })
    {
        time = throws ? deadline : DateTimeOffset.MaxValue;
        clockThrows = throws;
        var unavailable = manager.State;
        Check(unavailable.Status == LicenseStatus.Unlicensed && manager.Plan == unavailable.Plan && manager.NeedsRefresh());
        Check(await observer.MoveNextAsync() && observer.Current == unavailable);
        await using var recovery = manager.WatchStatesAsync(timeout.Token).GetAsyncEnumerator();
        Check(await recovery.MoveNextAsync() && recovery.Current == unavailable);
        clockThrows = false;
        time = deadline;
        Check(manager.State == available);
        Check(await observer.MoveNextAsync() && observer.Current == available);
        Check(await recovery.MoveNextAsync() && recovery.Current == available);
    }
    Check(clockProvider.ActivationCount == 1 && clockProvider.ValidationCount == 0 && clockProvider.ReleaseCount == 0);
    Check((clockStorage.LoadCount, clockStorage.SaveCount) == (beforeQueries.LoadCount, beforeQueries.SaveCount));
}
{
    var time = clock();
    using var manager = new LicenseManager(new ReleaseProvider(), new MemoryStorage(), clock: clock);
    LicenseState Snapshot(string source, string plan) => manager.AdoptExternallyVerifiedActivation(new(source, plan, time));
    const string composed = "caf\u00e9";
    const string decomposed = "cafe\u0301";
    var original = Snapshot(composed, composed);
    var sourceChange = Snapshot(decomposed, composed);
    var planChange = Snapshot(composed, decomposed);
    Check(original.Source != sourceChange.Source && original.Plan == sourceChange.Plan);
    Check(original.Source == planChange.Source && original.Plan != planChange.Plan);
    Check(new HashSet<LicenseState> { original, sourceChange, planChange }.Count == 3);
}
if (OperatingSystem.IsWindowsVersionAtLeast(10, 0, 26100))
{
    var directory = Path.Combine(Path.GetTempPath(), "software-licensing-fixture-" + Guid.NewGuid().ToString("N"));
    try
    {
        var secure = new WindowsLicenseStorage(directory, "com.example.software-licensing.fixture");
        var bytes = Encoding.UTF8.GetBytes("""{"version":1,"secret":"FIXTURE-KEY"}""");
        secure.Save(bytes);
        Check(JsonElement.DeepEquals(JsonDocument.Parse(secure.Load()!).RootElement, JsonDocument.Parse(bytes).RootElement));
        Check(!Encoding.UTF8.GetString(File.ReadAllBytes(Path.Combine(directory, "license.dpapi"))).Contains("FIXTURE-KEY"));
        ExpectCode(() => secure.Save("{"u8.ToArray()), LicenseErrorCode.InvalidData);
        ExpectCode(() => secure.Save("null"u8.ToArray()), LicenseErrorCode.InvalidData);
        try { new WindowsLicenseStorage(directory, "another.namespace").Load(); throw new System.Diagnostics.UnreachableException("Expected namespace mismatch"); }
        catch (LicenseException e) { Check(e.Failure.Code == LicenseErrorCode.StorageFailure); }
        Check(JsonElement.DeepEquals(JsonDocument.Parse(secure.Load()!).RootElement, JsonDocument.Parse(bytes).RootElement));
        secure.Delete();
        Check(secure.Load() is null);
    }
    finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
    Console.WriteLine("Windows user-scoped DPAPI round trip, namespace binding and cleanup passed.");
}
foreach (var authoritativelyRejected in new[] { false, true })
{
    var validationStorage = new MemoryStorage();
    var validationProvider = new ValidationProvider();
    if (authoritativelyRejected) validationProvider.Result = new(false);
    using var manager = new LicenseManager(validationProvider, validationStorage, clock: clock);
    await manager.ActivateAsync(new("TEST-KEY"));
    var savedRecord = validationStorage.Data!.ToArray();
    var expectedStatus = authoritativelyRejected ? LicenseStatus.Invalid : LicenseStatus.GracePeriod;
    validationStorage.Fails = true;
    await ExpectStorageFailure(manager.RefreshAsync());
    Check(validationStorage.Data!.AsSpan().SequenceEqual(savedRecord));
    Check(manager.State.Status == expectedStatus && manager.State.Plan.IsLicensed == !authoritativelyRejected);
    Check(manager.State.HasActivation && manager.State.HasPendingPersistence);
    validationStorage.Fails = false;
    await manager.RetryPersistenceAsync();
    Check(manager.State.Status == expectedStatus && !manager.State.HasPendingPersistence);
    Check(validationProvider.Validations == 1 && !validationStorage.Data!.AsSpan().SequenceEqual(savedRecord));
}
Console.WriteLine(".NET lifecycle, storage retry, deactivation recovery, unsupported records and generic provider port passed.");

// Activation has two writes: intent before the request, then the provider's result.
var journalStorage = new MemoryStorage { Fails = true };
var journalProvider = new ReleaseProvider();
using (var journal = new LicenseManager(journalProvider, journalStorage, clock: clock))
{
    await ExpectStorageFailure(journal.ActivateAsync(new("TEST-KEY")));
    Check(journalProvider.ActivationCount == 0 && journalStorage.Data is null);
    Check(journal.State.HasPendingPersistence && !journal.State.Plan.IsLicensed);
    journalProvider.BeforeActivate = () =>
    {
        using var intent = JsonDocument.Parse(journalStorage.Data!);
        Check(intent.RootElement.GetProperty("version").GetInt32() == 2);
        Check(intent.RootElement.GetProperty("pendingActivation").GetProperty("licenseKey").GetString() == "TEST-KEY");
    };
    journalStorage.Fails = false;
    await journal.RetryPersistenceAsync();
    await journal.RetryPersistenceAsync();
    Check(journalProvider.ActivationCount == 1 && journalStorage.SaveCount == 3);
    Check(journal.State.Plan.IsLicensed && !journal.State.HasPendingActivation);
}
var beforeActivationStorage = new MemoryStorage();
var beforeActivationProvider = new ReleaseProvider();
using (var beforeActivation = new LicenseManager(beforeActivationProvider, beforeActivationStorage, clock: clock))
{
    using var cancellation = new CancellationTokenSource();
    beforeActivationStorage.BeforeSave = cancellation.Cancel;
    await ExpectCancellation(beforeActivation.ActivateAsync(new("TEST-KEY"), cancellation.Token));
    Check(beforeActivationProvider.ActivationCount == 0 && beforeActivationStorage.SaveCount == 2);
    Check(!beforeActivation.State.HasPendingPersistence && !beforeActivation.State.HasPendingActivation);
    Check(beforeActivation.State.LastOperationFailure == new LicenseFailure(LicenseErrorCode.Cancelled,
        MutationOutcome: LicenseMutationOutcome.NotApplied));
    using var restored = new LicenseManager(beforeActivationProvider, beforeActivationStorage, clock: clock);
    Check(restored.State.LastOperationFailure == beforeActivation.State.LastOperationFailure);
    Check(!restored.State.HasPendingActivation && !restored.State.HasActivation);
}
{
    var failureStorage = new MemoryStorage { FailOnSave = 2 };
    var original = new LicenseFailure(LicenseErrorCode.ServerFailure, 503, 120, LicenseMutationOutcome.Unknown);
    var failureProvider = new ReleaseProvider
    {
        ActivationFailure = new LicenseException(original),
        BeforeActivate = () => Check(failureStorage.SaveCount == 1 && failureStorage.Data is not null),
    };
    using var failed = new LicenseManager(failureProvider, failureStorage, clock: clock);
    await ExpectStorageFailure(failed.ActivateAsync(new("TEST-KEY")));
    Check(failed.State.HasPendingPersistence && failureStorage.SaveCount == 2);
    await using var states = failed.WatchStatesAsync().GetAsyncEnumerator();
    Check(await states.MoveNextAsync().AsTask().WaitAsync(TimeSpan.FromSeconds(5)) && states.Current.HasPendingPersistence);
    failureStorage.FailOnSave = null;
    await ExpectFailure(failed.RetryPersistenceAsync(), original with { Code = LicenseErrorCode.MutationOutcomeUnknown });
    Check(await states.MoveNextAsync().AsTask().WaitAsync(TimeSpan.FromSeconds(5)));
    Check(!states.Current.HasPendingPersistence && states.Current.HasPendingActivation);
    Check(states.Current.LastOperationFailure == original);
    await failed.RetryPersistenceAsync();

    using var restored = new LicenseManager(failureProvider, failureStorage, clock: clock);
    Check(restored.State.LastOperationFailure == original);
    await ExpectFailure(restored.ActivateAsync(new("TEST-KEY")), new LicenseFailure(LicenseErrorCode.ActivationPending));
    Check(failureProvider.ActivationCount == 1 && failureProvider.ValidationCount == 0 && failureProvider.ReleaseCount == 0);
    Check(failureStorage.SaveCount == 3);
}
// Cancellation after provider entry is unknown unless the provider supplies proof.
foreach (var kind in new[] { "plain", "unknown", "notApplied" })
{
    var original = new LicenseFailure(LicenseErrorCode.Cancelled, RetryAfterSeconds: kind == "plain" ? null : 90,
        MutationOutcome: kind == "plain" ? null : kind == "unknown" ? LicenseMutationOutcome.Unknown : LicenseMutationOutcome.NotApplied);
    var cancellationProvider = new ReleaseProvider
    {
        ActivationFailure = kind == "plain" ? new OperationCanceledException() : new LicenseException(original),
    };
    var cancellationStorage = new MemoryStorage();
    using var cancelled = new LicenseManager(cancellationProvider, cancellationStorage, clock: clock);
    if (kind == "notApplied") await ExpectCancellation(cancelled.ActivateAsync(new("TEST-KEY")));
    else await ExpectFailure(cancelled.ActivateAsync(new("TEST-KEY")), original with
    {
        Code = LicenseErrorCode.MutationOutcomeUnknown,
        MutationOutcome = LicenseMutationOutcome.Unknown,
    });
    Check(cancelled.State.LastOperationFailure == original && cancelled.State.HasPendingActivation == (kind != "notApplied"));
    Check(cancellationStorage.SaveCount == 2);
    using var restored = new LicenseManager(cancellationProvider, cancellationStorage, clock: clock);
    if (kind != "notApplied")
    {
        // Fixture authority established externally that this saved request did not apply.
        restored.ConfirmActivationNotAppliedAfterExternalVerification();
        Check(restored.State.LastOperationFailure is null);
    }
    Check(!restored.State.HasPendingActivation && cancellationProvider.ActivationCount == 1);
    Check(cancellationProvider.ValidationCount == 0 && cancellationProvider.ReleaseCount == 0);
}
var cancelledValidationStorage = new MemoryStorage();
var cancelledValidationProvider = new ReleaseProvider();
using (var cancelledValidation = new LicenseManager(cancelledValidationProvider, cancelledValidationStorage,
    clock: () => DateTimeOffset.FromUnixTimeSeconds(2000)))
using (var cancellation = new CancellationTokenSource())
{
    await cancelledValidation.ActivateAsync(new("TEST-KEY"));
    var saves = cancelledValidationStorage.SaveCount;
    var nextRefresh = cancelledValidation.State.NextRefreshAt;
    cancelledValidationProvider.BeforeValidate = cancellation.Cancel;
    cancelledValidationProvider.ValidationFailure = new(LicenseErrorCode.RateLimited, 429, 120);
    try
    {
        await cancelledValidation.RefreshAsync(cancellationToken: cancellation.Token);
        throw new InvalidOperationException("Expected cancelled validation");
    }
    catch (OperationCanceledException) { }
    Check(cancelledValidationStorage.SaveCount == saves);
    Check(cancelledValidation.State.NextRefreshAt == nextRefresh && !cancelledValidation.State.IsRefreshing);
}
var cooldownStorage = new MemoryStorage();
var cooldownProvider = new ReleaseProvider();
var cooldownNow = DateTimeOffset.FromUnixTimeSeconds(2000);
var cooldownPolicy = new LicenseRefreshPolicy(true, 3600, 600, 300);
Check(cooldownPolicy.FailureRetryInterval == 60);
using (var cooldown = new LicenseManager(cooldownProvider, cooldownStorage, cooldownPolicy, clock: () => cooldownNow))
{
    await cooldown.ActivateAsync(new("TEST-KEY"));
    cooldownProvider.ValidationFailure = new(LicenseErrorCode.RateLimited, 429, 120);
    Check((await cooldown.RefreshAsync()).Outcome == LicenseRefreshOutcome.GracePeriod);
    Check(cooldownProvider.ValidationCount == 1 && !cooldown.NeedsRefresh());
    Check(cooldown.State.NextRefreshAt == DateTimeOffset.FromUnixTimeSeconds(2120));
    var saves = cooldownStorage.SaveCount;
    using var restored = new LicenseManager(cooldownProvider, cooldownStorage, cooldownPolicy, clock: () => cooldownNow);
    Check(cooldownStorage.SaveCount == saves && restored.State.NextRefreshAt == cooldown.State.NextRefreshAt);
    Check((await restored.RefreshAsync(onlyIfDue: true)).Outcome == LicenseRefreshOutcome.SkippedNotDue);
    Check(cooldownProvider.ValidationCount == 1 && cooldownStorage.SaveCount == saves);
    cooldownNow = DateTimeOffset.FromUnixTimeSeconds(2250);
    cooldownProvider.ValidationResult = new(true);
    Check((await restored.RefreshAsync(onlyIfDue: true)).Outcome == LicenseRefreshOutcome.Validated);
    Check(cooldownProvider.ValidationCount == 2);
    cooldownProvider.ReleaseFailure = new(LicenseErrorCode.TransportFailure, RetryAfterSeconds: 90);
    await ExpectFailure(restored.DeactivateAsync(), new(LicenseErrorCode.MutationOutcomeUnknown,
        RetryAfterSeconds: 90, MutationOutcome: LicenseMutationOutcome.Unknown));
    Check(restored.State.HasPendingDeactivation && cooldownProvider.ReleaseCount == 1);
    var releaseSaves = cooldownStorage.SaveCount;
    Check((await restored.RefreshAsync(onlyIfDue: true)).Outcome == LicenseRefreshOutcome.SkippedNotDue);
    Check(cooldownProvider.ReleaseCount == 1 && cooldownStorage.SaveCount == releaseSaves);
    cooldownNow = DateTimeOffset.FromUnixTimeSeconds(2500);
    cooldownProvider.ReleaseFailure = null;
    Check((await restored.RefreshAsync(onlyIfDue: true)).Outcome == LicenseRefreshOutcome.Deactivated);
    Check(cooldownProvider.ReleaseCount == 2 && !restored.State.HasActivation);
}
{
    var verifiedReleaseStorage = new MemoryStorage
    {
        Data = """
            {"version":2,"status":"deactivated","pendingDeactivation":true,
             "activation":{"source":"fixture","planIdentifier":"paid","activatedAt":1000,
                           "activationIdentifier":"fixture-instance"},
             "lastOperationFailure":{"code":"serverFailure"}}
            """u8.ToArray(),
    };
    var verifiedReleaseProvider = new ReleaseProvider();
    using var restored = new LicenseManager(verifiedReleaseProvider, verifiedReleaseStorage, clock: clock);
    Check(restored.State.HasPendingDeactivation);
    // Fixture authority confirmed release of the saved remote instance externally.
    restored.ConfirmDeactivationAfterExternalVerification();
    Check(verifiedReleaseStorage.SaveCount == 1);
    Check((verifiedReleaseProvider.ActivationCount, verifiedReleaseProvider.ValidationCount, verifiedReleaseProvider.ReleaseCount) == (0, 0, 0));
    Check(!restored.State.HasActivation && !restored.State.HasPendingDeactivation && restored.State.LastOperationFailure is null);
}
var legacyStorage = new MemoryStorage();
var legacyProvider = new ReleaseProvider();
using (var current = new LicenseManager(legacyProvider, legacyStorage, clock: clock))
{
    await current.ActivateAsync(new("TEST-KEY"));
    var legacy = JsonNode.Parse(legacyStorage.Data!)!.AsObject();
    legacy["version"] = 1;
    foreach (var name in new[] { "observedAt", "nextRefreshAt", "pendingActivation", "lastOperationFailure" }) legacy.Remove(name);
    var bytes = Encoding.Unicode.GetBytes("\uFEFF" + legacy.ToJsonString());
    legacyStorage.Data = bytes.ToArray();
    var saves = legacyStorage.SaveCount;
    using var restored = new LicenseManager(legacyProvider, legacyStorage, clock: clock);
    Check(restored.InitialRestoreError is null && restored.State.Plan.IsLicensed && restored.State.Plan == current.State.Plan);
    Check(legacyStorage.Data!.AsSpan().SequenceEqual(bytes) && legacyStorage.SaveCount == saves);
    Check((legacyProvider.ActivationCount, legacyProvider.ValidationCount, legacyProvider.ReleaseCount) == (1, 0, 0));
}
// Host clocks can re-enter from another thread too; no callback runs under gate.
LicenseManager? clockManager = null;
var inspectClock = 0;
DateTimeOffset ReentrantClock()
{
    if (Interlocked.Exchange(ref inspectClock, 0) == 1)
        Check(Task.Run(() => clockManager!.State).Wait(TimeSpan.FromSeconds(5)));
    return DateTimeOffset.FromUnixTimeSeconds(2000);
}
using (clockManager = new LicenseManager(new ReleaseProvider(), new MemoryStorage(), clock: ReentrantClock))
{
    Interlocked.Exchange(ref inspectClock, 1);
    Check(!clockManager.State.HasActivation);
}
Console.WriteLine(".NET journal ordering, durable mutation errors, external reconciliation and persisted cooldowns passed.");

// A completed save (successful or failed) must keep its actual receipt when the
// acknowledgement clock fails. Recovery must not repeat successful writes.
foreach (var fault in new[] { "throw", "range" })
    foreach (var stage in new[] { "activationIntent", "activationResult", "releaseMarker", "releaseResult", "validationResult" })
        foreach (var failedSave in new[] { false, true })
        {
            var completionClock = new CompletionClock();
            var completionStorage = new MemoryStorage();
            var completionProvider = new ReleaseProvider { ValidationResult = new(true) };
            using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
            var release = stage is "releaseMarker" or "releaseResult";
            if (release || stage == "validationResult") await manager.ActivateAsync(new("TEST-KEY"));
            var targetSave = completionStorage.SaveCount + (stage is "activationResult" or "releaseResult" ? 2 : 1);
            if (failedSave) completionStorage.FailOnSave = targetSave;
            void AfterSave()
            {
                if (completionStorage.SaveCount == targetSave) completionClock.Fault = fault;
                else completionStorage.AfterSave = AfterSave;
            }
            completionStorage.AfterSave = AfterSave;
            await ExpectFailure(release ? manager.DeactivateAsync() : stage == "validationResult"
                ? manager.RefreshAsync() : manager.ActivateAsync(new("TEST-KEY")), new(LicenseErrorCode.InvalidData));
            var calls = (completionProvider.ActivationCount, completionProvider.ValidationCount, completionProvider.ReleaseCount);
            Check(completionStorage.SaveCount == targetSave);
            // Repeated faults leave the same receipt outstanding without further IO.
            await ExpectFailure(manager.RetryPersistenceAsync(), new(LicenseErrorCode.InvalidData));
            Check(completionStorage.SaveCount == targetSave);
            Check(calls == (completionProvider.ActivationCount, completionProvider.ValidationCount, completionProvider.ReleaseCount));
            Check(manager.State.Status == LicenseStatus.Unlicensed && manager.NeedsRefresh());
            ExpectCode(manager.Dispose, LicenseErrorCode.PersistencePending);
            completionClock.Fault = null;
            Check(manager.State.HasPendingPersistence && manager.State.Plan.IsLicensed == (stage == "validationResult"));
            ExpectCode(manager.Dispose, LicenseErrorCode.PersistencePending);
            await CheckCancelledAdmission(manager, completionStorage, completionProvider);
            completionStorage.FailOnSave = null;
            await manager.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
            await manager.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
            Check(completionStorage.SaveCount == targetSave + (failedSave ? 1 : 0)
                + (stage is "activationIntent" or "releaseMarker" ? 1 : 0));
            Check(completionProvider.ActivationCount == 1 && completionProvider.ReleaseCount == (release ? 1 : 0));
            Check(completionProvider.ValidationCount == (stage == "validationResult" ? 1 : 0));
            Check(!manager.State.HasPendingPersistence && manager.State.HasActivation == !release);
            Check(manager.State.Plan.IsLicensed == !release);
        }

// Provider mutations that finish successfully survive both clock failure and
// late cancellation. Validation uses the same deferred completion path.
foreach (var fault in new[] { "throw", "range" })
    foreach (var operation in new[] { "activate", "release", "validate" })
    {
        var completionClock = new CompletionClock();
        var completionStorage = new MemoryStorage();
        var completionProvider = new ReleaseProvider { ValidationResult = new(true) };
        using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
        if (operation != "activate") await manager.ActivateAsync(new("TEST-KEY"));
        using var cancellation = new CancellationTokenSource();
        void Finish()
        {
            if (operation != "validate") cancellation.Cancel();
            completionClock.Fault = fault;
        }
        if (operation == "activate") completionProvider.BeforeActivate = Finish;
        else if (operation == "release") completionProvider.BeforeRelease = Finish;
        else completionProvider.BeforeValidate = Finish;
        await ExpectFailure(operation == "activate" ? manager.ActivateAsync(new("TEST-KEY"), cancellation.Token)
            : operation == "release" ? manager.DeactivateAsync(cancellation.Token) : manager.RefreshAsync(),
            new(LicenseErrorCode.InvalidData));
        var saves = completionStorage.SaveCount;
        await ExpectFailure(manager.RetryPersistenceAsync(), new(LicenseErrorCode.InvalidData));
        Check(completionStorage.SaveCount == saves);
        Check(manager.State.Status == LicenseStatus.Unlicensed && manager.NeedsRefresh());
        ExpectCode(manager.Dispose, LicenseErrorCode.OperationInProgress);
        completionClock.Fault = null;
        ExpectCode(manager.Dispose, LicenseErrorCode.OperationInProgress);
        await CheckCancelledAdmission(manager, completionStorage, completionProvider);
        await manager.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
        await manager.RetryPersistenceAsync().WaitAsync(TimeSpan.FromSeconds(5));
        Check(completionProvider.ActivationCount == 1 && completionProvider.ReleaseCount == (operation == "release" ? 1 : 0));
        Check(completionProvider.ValidationCount == (operation == "validate" ? 1 : 0));
        Check(completionStorage.SaveCount == saves + 1 && !manager.State.HasPendingPersistence);
        Check(manager.State.HasActivation == (operation != "release") && manager.State.Plan.IsLicensed == (operation != "release"));
        using var saved = JsonDocument.Parse(completionStorage.Data!);
        Check((saved.RootElement.GetProperty("activation").ValueKind == JsonValueKind.Null) == (operation == "release"));
    }

// Failure metadata belongs to the completed provider result, including the
// distinction between a known unapplied mutation and an unknown outcome.
foreach (var fault in new[] { "throw", "range" })
    foreach (var outcome in new[] { LicenseMutationOutcome.NotApplied, LicenseMutationOutcome.Unknown })
    {
        var completionClock = new CompletionClock();
        var original = new LicenseFailure(LicenseErrorCode.ServerFailure, 503, 120, outcome);
        var completionProvider = new ReleaseProvider
        {
            ActivationFailure = new LicenseException(original),
            BeforeActivate = () => completionClock.Fault = fault,
        };
        var completionStorage = new MemoryStorage();
        using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
        await ExpectFailure(manager.ActivateAsync(new("TEST-KEY")), new(LicenseErrorCode.InvalidData));
        completionClock.Fault = null;
        var reported = outcome == LicenseMutationOutcome.Unknown
            ? original with { Code = LicenseErrorCode.MutationOutcomeUnknown } : original;
        await ExpectFailure(manager.RetryPersistenceAsync(), reported);
        await manager.RetryPersistenceAsync();
        Check(manager.State.LastOperationFailure == original && !manager.State.HasPendingPersistence);
        Check(manager.State.HasPendingActivation == (outcome == LicenseMutationOutcome.Unknown));
        Check(completionProvider.ActivationCount == 1 && completionStorage.SaveCount == 2);
    }

// Every normal recovery entry must replay a queued result before beginning its
// requested operation. Only that new operation may contact the provider again.
foreach (var entry in new[] { "activate", "refresh", "deactivate" })
{
    var completionClock = new CompletionClock();
    var completionProvider = new ReleaseProvider
    {
        BeforeActivate = () => completionClock.Fault = "throw",
        ValidationResult = new(true),
    };
    var completionStorage = new MemoryStorage();
    using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
    await ExpectFailure(manager.ActivateAsync(new("TEST-KEY")), new(LicenseErrorCode.InvalidData));
    completionClock.Fault = null;
    if (entry == "activate") await ExpectFailure(manager.ActivateAsync(new("ANOTHER-KEY")), new(LicenseErrorCode.ActivationAlreadyPresent));
    else if (entry == "refresh") Check((await manager.RefreshAsync()).Outcome == LicenseRefreshOutcome.Validated);
    else await manager.DeactivateAsync();
    Check(completionProvider.ActivationCount == 1 && completionProvider.ValidationCount == (entry == "refresh" ? 1 : 0));
    Check(completionProvider.ReleaseCount == (entry == "deactivate" ? 1 : 0) && !manager.State.HasPendingPersistence);
}

// A live driver must keep its completion private while acquiring time. A clock
// can re-enter from another thread before eventually throwing/returning bad time.
foreach (var fault in new[] { "throw", "range" })
{
    var completionClock = new CompletionClock();
    var completionProvider = new ReleaseProvider();
    var completionStorage = new MemoryStorage();
    using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
    var reentered = false;
    completionStorage.AfterSave = () => completionClock.BeforeRead = () =>
    {
        Check(Task.Run(() => manager.State).Wait(TimeSpan.FromSeconds(5)));
        ExpectFailure(Task.Run(() => manager.RetryPersistenceAsync()), new(LicenseErrorCode.OperationInProgress)).GetAwaiter().GetResult();
        reentered = true;
        completionClock.Fault = fault;
    };
    await ExpectFailure(manager.ActivateAsync(new("TEST-KEY")), new(LicenseErrorCode.InvalidData));
    Check(reentered && completionStorage.SaveCount == 1 && completionProvider.ActivationCount == 0);
    completionClock.Fault = null;
    await manager.RetryPersistenceAsync();
    Check(completionStorage.SaveCount == 2 && completionProvider.ActivationCount == 1 && manager.State.Plan.IsLicensed);
}

// A single owner resumes the receipt and drives its follow-on provider/save
// effects. Competitors and host callback reentrancy must never duplicate IO.
foreach (var fault in new[] { "throw", "range" })
    foreach (var failedSave in new[] { false, true })
    {
        var completionClock = new CompletionClock();
        var completionStorage = new MemoryStorage { FailOnSave = failedSave ? 2 : null };
        var completionProvider = new ReleaseProvider();
        using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
        if (failedSave)
        {
            void AfterSave()
            {
                if (completionStorage.SaveCount == 2) completionClock.Fault = fault;
                else completionStorage.AfterSave = AfterSave;
            }
            completionStorage.AfterSave = AfterSave;
        }
        else completionProvider.BeforeActivate = () => completionClock.Fault = fault;
        await ExpectFailure(manager.ActivateAsync(new("TEST-KEY")), new(LicenseErrorCode.InvalidData));
        completionClock.Fault = null;
        completionStorage.FailOnSave = null;
        using var entered = new ManualResetEventSlim();
        using var proceed = new ManualResetEventSlim();
        completionClock.BeforeRead = () =>
        {
            entered.Set();
            Check(proceed.Wait(TimeSpan.FromSeconds(5)));
        };
        var reenteredSave = false;
        completionStorage.BeforeSave = () =>
        {
            Check(Task.Run(() => manager.State).Wait(TimeSpan.FromSeconds(5)));
            ExpectFailure(Task.Run(() => manager.RetryPersistenceAsync()), new(LicenseErrorCode.OperationInProgress)).GetAwaiter().GetResult();
            reenteredSave = true;
        };
        var resume = Task.Run(() => manager.RetryPersistenceAsync());
        try
        {
            Check(entered.Wait(TimeSpan.FromSeconds(5)));
            await ExpectFailure(manager.RetryPersistenceAsync(), new(LicenseErrorCode.OperationInProgress));
            await ExpectFailure(manager.RefreshAsync(), new(LicenseErrorCode.OperationInProgress));
            await CheckCancelledAdmission(manager, completionStorage, completionProvider);
            Check(completionProvider.ActivationCount == 1 && completionStorage.SaveCount == (failedSave ? 2 : 1));
        }
        finally { proceed.Set(); }
        await resume.WaitAsync(TimeSpan.FromSeconds(5));
        Check(reenteredSave && completionProvider.ActivationCount == 1 && completionStorage.SaveCount == (failedSave ? 3 : 2));
        Check(manager.State.Plan.IsLicensed && !manager.State.HasPendingPersistence);
    }

// A reload acquiring time must not discard a provider completion it overlaps.
{
    var completionClock = new CompletionClock();
    var completionStorage = new MemoryStorage();
    var completionProvider = new ReleaseProvider
    {
        PendingActivation = new(TaskCreationOptions.RunContinuationsAsynchronously),
    };
    using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
    var activation = manager.ActivateAsync(new("TEST-KEY"));
    using var entered = new ManualResetEventSlim();
    using var proceed = new ManualResetEventSlim();
    completionClock.BeforeRead = () =>
    {
        entered.Set();
        Check(proceed.Wait(TimeSpan.FromSeconds(5)));
    };
    var reload = Task.Run(manager.Reload);
    try
    {
        Check(entered.Wait(TimeSpan.FromSeconds(5)));
        completionProvider.PendingActivation.SetResult(await new Provider(false).ActivateAsync(new("TEST-KEY")));
        Check((await activation.WaitAsync(TimeSpan.FromSeconds(5))).Plan.IsLicensed);
    }
    finally { proceed.Set(); }
    await reload.WaitAsync(TimeSpan.FromSeconds(5));
    Check(manager.State.Plan.IsLicensed && completionStorage.SaveCount == 2 && completionProvider.ActivationCount == 1);
}

// Ownership also spans an asynchronous provider emitted by a resumed intent
// receipt. Read-only access and already-cancelled calls remain available.
foreach (var fault in new[] { "throw", "range" })
{
    var completionClock = new CompletionClock();
    var completionStorage = new MemoryStorage { AfterSave = () => completionClock.Fault = fault };
    var completionProvider = new ReleaseProvider
    {
        PendingActivation = new(TaskCreationOptions.RunContinuationsAsynchronously),
    };
    using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
    await ExpectFailure(manager.ActivateAsync(new("TEST-KEY")), new(LicenseErrorCode.InvalidData));
    completionClock.Fault = null;
    var resume = manager.RetryPersistenceAsync();
    Check(!resume.IsCompleted && manager.State.IsActivating && completionProvider.ActivationCount == 1);
    await ExpectFailure(manager.RetryPersistenceAsync(), new(LicenseErrorCode.OperationInProgress));
    await ExpectFailure(manager.DeactivateAsync(), new(LicenseErrorCode.OperationInProgress));
    await CheckCancelledAdmission(manager, completionStorage, completionProvider);
    completionProvider.PendingActivation.SetResult(await new Provider(false).ActivateAsync(new("TEST-KEY")));
    await resume.WaitAsync(TimeSpan.FromSeconds(5));
    Check(completionProvider.ActivationCount == 1 && completionStorage.SaveCount == 2 && manager.State.Plan.IsLicensed);
}

// Recovery can itself encounter the next clock fault. Each accepted receipt is
// replaced only by the next completed effect; no finished IO is driven twice.
{
    var completionClock = new CompletionClock();
    var completionStorage = new MemoryStorage { AfterSave = () => completionClock.Fault = "throw" };
    var completionProvider = new ReleaseProvider { BeforeActivate = () => completionClock.Fault = "range" };
    using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
    await ExpectFailure(manager.ActivateAsync(new("TEST-KEY")), new(LicenseErrorCode.InvalidData));
    completionClock.Fault = null;
    await ExpectFailure(manager.RetryPersistenceAsync(), new(LicenseErrorCode.InvalidData));
    Check(completionStorage.SaveCount == 1 && completionProvider.ActivationCount == 1);
    completionClock.Fault = null;
    completionStorage.AfterSave = () => completionClock.Fault = "throw";
    await ExpectFailure(manager.RetryPersistenceAsync(), new(LicenseErrorCode.InvalidData));
    Check(completionStorage.SaveCount == 2 && completionProvider.ActivationCount == 1);
    completionClock.Fault = null;
    await manager.RetryPersistenceAsync();
    Check(completionStorage.SaveCount == 2 && completionProvider.ActivationCount == 1 && manager.State.Plan.IsLicensed);
}

// A clock callback can cancel recovery after a failed write. Acknowledgement
// makes the write retryable without issuing another write to a cancelled host.
{
    var completionClock = new CompletionClock();
    var completionProvider = new ReleaseProvider();
    var completionStorage = new MemoryStorage
    {
        FailOnSave = 1,
        AfterSave = () => completionClock.Fault = "throw",
    };
    using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
    await ExpectFailure(manager.ActivateAsync(new("TEST-KEY")), new(LicenseErrorCode.InvalidData));
    completionStorage.FailOnSave = null;
    completionClock.Fault = null;
    using var cancellation = new CancellationTokenSource();
    completionClock.BeforeRead = cancellation.Cancel;
    await ExpectCancellation(manager.RetryPersistenceAsync(cancellation.Token));
    Check(manager.State.HasPendingPersistence && completionStorage.SaveCount == 1 && completionProvider.ActivationCount == 0);
    await manager.RetryPersistenceAsync();
    Check(manager.State.Plan.IsLicensed && completionStorage.SaveCount == 3 && completionProvider.ActivationCount == 1);
}

// Invalid provider payloads are a different failure: at valid time, cancel the
// original operation using that same time rather than queuing bad data forever.
foreach (var (deferFirst, oversized) in new[] { (false, false), (true, false), (false, true), (true, true) })
{
    var completionClock = new CompletionClock();
    var completionProvider = new ReleaseProvider
    {
        ActivationResponse = () => oversized
            ? new(new string('x', 1_048_576), "paid", DateTimeOffset.UnixEpoch)
            : null!,
    };
    var completionStorage = new MemoryStorage();
    using var manager = new LicenseManager(completionProvider, completionStorage, clock: completionClock.Read);
    completionProvider.BeforeActivate = () =>
    {
        if (deferFirst) completionClock.Fault = "range";
        else completionClock.BeforeRead = () => completionClock.FaultOnNextRead = "throw";
    };
    await ExpectFailure(manager.ActivateAsync(new("TEST-KEY")), new(LicenseErrorCode.InvalidData));
    if (deferFirst)
    {
        completionClock.Fault = null;
        completionClock.BeforeRead = () => completionClock.FaultOnNextRead = "throw";
        await ExpectFailure(manager.RetryPersistenceAsync(), new(LicenseErrorCode.InvalidData));
    }
    // Neither completion nor cancel may acquire time a second time.
    Check(completionClock.Fault == "throw" && completionStorage.SaveCount == 1 && completionProvider.ActivationCount == 1);
    completionClock.Fault = null;
    Check(!manager.State.IsActivating && !manager.State.HasPendingPersistence && manager.State.HasPendingActivation);
    await manager.RetryPersistenceAsync();
    Check(completionStorage.SaveCount == 1 && completionProvider.ActivationCount == 1);
    // Fixture authority established that the malformed request did not apply.
    manager.ConfirmActivationNotAppliedAfterExternalVerification();
}
// Resource lifetime does not depend on a working licensing clock. Cached Rust
// operation flags still prevent disposal from discarding outstanding effects.
foreach (var fault in new[] { "throw", "range" })
    foreach (var state in new[] { "empty", "licensed", "unreadable" })
    {
        var disposalClock = new CompletionClock();
        var disposalStorage = new MemoryStorage();
        if (state == "unreadable") disposalStorage.Data = Encoding.UTF8.GetBytes("unreadable record");
        var manager = new LicenseManager(new ReleaseProvider(), disposalStorage, clock: disposalClock.Read);
        if (state == "licensed") await manager.ActivateAsync(new("TEST-KEY"));
        Check((manager.InitialRestoreError is not null) == (state == "unreadable"));
        var calls = (disposalStorage.LoadCount, disposalStorage.SaveCount);
        var readClock = false;
        disposalClock.Fault = fault;
        disposalClock.BeforeRead = () => readClock = true;
        manager.Dispose();
        manager.Dispose();
        Check(!readClock && calls == (disposalStorage.LoadCount, disposalStorage.SaveCount));
    }
foreach (var duringLoad in new[] { false, true })
{
    foreach (var subsecond in new[] { false, true })
    {
        var current = DateTimeOffset.FromUnixTimeSeconds(2000);
        var observedAt = DateTimeOffset.FromUnixTimeSeconds(3500);
        var rolledBack = subsecond ? observedAt.AddMilliseconds(-500) : DateTimeOffset.FromUnixTimeSeconds(2500);
        var reloadStorage = new MemoryStorage();
        var reloadProvider = new ReleaseProvider();
        using var reloadManager = new LicenseManager(reloadProvider, reloadStorage, clock: () => current);
        reloadManager.AdoptExternallyVerifiedActivation(new(
            "fixture", "paid", DateTimeOffset.FromUnixTimeSeconds(1000),
            ExpiresAt: subsecond ? observedAt.AddMilliseconds(-250) : DateTimeOffset.FromUnixTimeSeconds(3000)));
        current = observedAt;
        Check(reloadManager.State.Status == LicenseStatus.Expired);
        var saved = reloadStorage.Data;
        var loads = reloadStorage.LoadCount;
        if (duringLoad) reloadStorage.BeforeLoad = () => current = rolledBack;
        else current = rolledBack;
        ExpectCode(reloadManager.Reload, LicenseErrorCode.ClockRollback);
        Check(!reloadManager.State.Plan.IsLicensed && ReferenceEquals(saved, reloadStorage.Data));
        Check(reloadStorage.LoadCount == loads + (duringLoad ? 1 : 0));
        current = observedAt;
        reloadManager.Reload();
        Check(reloadManager.State.Status == LicenseStatus.Expired);
    }
}
// Both clock samples and the storage read run while reload owns replacement.
foreach (var callbackPhase in new[] { "first clock", "storage", "final clock" })
{
    var reloadClock = new CompletionClock();
    var observedReloadStorage = new MemoryStorage();
    var observedReloadProvider = new ReleaseProvider();
    using var observedReload = new LicenseManager(observedReloadProvider, observedReloadStorage, clock: reloadClock.Read);
    observedReload.AdoptExternallyVerifiedActivation(new("fixture", "original", DateTimeOffset.FromUnixTimeSeconds(1000)));
    var original = observedReload.State;
    var needsRefresh = observedReload.NeedsRefresh();
    var replacementStorage = new MemoryStorage();
    using (var replacement = new LicenseManager(new ReleaseProvider(), replacementStorage, clock: clock))
        replacement.AdoptExternallyVerifiedActivation(new("fixture", "replacement", DateTimeOffset.FromUnixTimeSeconds(1000)));
    observedReloadStorage.Data = replacementStorage.Data;
    var saved = observedReloadStorage.Data;
    var saves = observedReloadStorage.SaveCount;
    var loads = observedReloadStorage.LoadCount;
    var observedDuringReload = false;
    void ObserveReload()
    {
        Check(observedReload.State == original && observedReload.NeedsRefresh() == needsRefresh);
        var queried = Task.Run(() => (observedReload.State, observedReload.NeedsRefresh()));
        Check(queried.Wait(TimeSpan.FromSeconds(5)) && queried.Result == (original, needsRefresh));
        var competed = Task.Run(() =>
        {
            ExpectCode(observedReload.Reload, LicenseErrorCode.OperationInProgress);
            ExpectCode(() => observedReload.AdoptExternallyVerifiedActivation(new(
                "fixture", "nested", DateTimeOffset.FromUnixTimeSeconds(1000))), LicenseErrorCode.OperationInProgress);
        });
        competed.WaitAsync(TimeSpan.FromSeconds(5)).GetAwaiter().GetResult();
        Check(ReferenceEquals(saved, observedReloadStorage.Data) && observedReloadStorage.SaveCount == saves);
        Check(observedReloadProvider.ActivationCount == 0 && observedReloadProvider.ValidationCount == 0 && observedReloadProvider.ReleaseCount == 0);
        observedDuringReload = true;
    }
    if (callbackPhase == "first clock")
        reloadClock.BeforeRead = ObserveReload;
    else if (callbackPhase == "storage")
        observedReloadStorage.BeforeLoad = ObserveReload;
    else
        observedReloadStorage.BeforeLoad = () => reloadClock.BeforeRead = ObserveReload;

    observedReload.Reload();
    Check(observedDuringReload && observedReload.State.Plan.Identifier == "replacement");
    Check(observedReloadStorage.LoadCount == loads + 1 && observedReloadStorage.SaveCount == saves);
    Check(ReferenceEquals(saved, observedReloadStorage.Data));
    Check(observedReloadProvider.ActivationCount == 0 && observedReloadProvider.ValidationCount == 0 && observedReloadProvider.ReleaseCount == 0);
}
// A faulty clock during initial restore must leave a usable blocked manager.
foreach (var fault in new[] { "throw", "range" })
{
    var startupStorage = new MemoryStorage();
    var startupProvider = new ReleaseProvider();
    using (var original = new LicenseManager(startupProvider, startupStorage, clock: clock))
        original.AdoptExternallyVerifiedActivation(new("fixture", "paid", DateTimeOffset.FromUnixTimeSeconds(1000)));
    var saved = startupStorage.Data;
    var loads = startupStorage.LoadCount;
    var saves = startupStorage.SaveCount;
    var startupClock = new CompletionClock { Fault = fault };
    var reads = 0;
    using var manager = new LicenseManager(startupProvider, startupStorage, clock: () =>
    {
        reads++;
        return startupClock.Read();
    });
    Check(reads == 1 && manager.InitialRestoreError == new LicenseFailure(LicenseErrorCode.InvalidData));
    Check(startupStorage.LoadCount == loads + 1 && startupStorage.SaveCount == saves);
    ExpectCode(manager.Reload, LicenseErrorCode.InvalidData);
    Check(startupStorage.LoadCount == loads + 1 && startupStorage.SaveCount == saves);
    startupClock.Fault = null;
    Check(manager.NeedsRefresh() && !manager.State.Plan.IsLicensed);
    await manager.RetryPersistenceAsync();
    Check(manager.InitialRestoreError is null && manager.State.Plan.IsLicensed);
    Check(ReferenceEquals(startupStorage.Data, saved) && startupStorage.LoadCount == loads + 2 && startupStorage.SaveCount == saves);
    Check(startupProvider.ActivationCount == 0 && startupProvider.ValidationCount == 0 && startupProvider.ReleaseCount == 0);
}
// A custom store may retain its returned array. Clock callbacks must not change
// the loaded candidate after parsing, for initial restore or an explicit reload.
foreach (var reload in new[] { false, true })
{
    var snapshotClock = new CompletionClock();
    var snapshotStorage = new MemoryStorage();
    var snapshotProvider = new ReleaseProvider();
    using var initial = new LicenseManager(snapshotProvider, snapshotStorage, clock: snapshotClock.Read);
    initial.AdoptExternallyVerifiedActivation(new("fixture", "fixture-one", DateTimeOffset.FromUnixTimeSeconds(1000)));
    var saved = snapshotStorage.Data!;
    var loadedSnapshot = saved.ToArray();
    var saves = snapshotStorage.SaveCount;
    var loads = snapshotStorage.LoadCount;
    bool mutated = false;
    snapshotStorage.BeforeLoad = () => snapshotClock.BeforeRead = () =>
    {
        MutateSyntheticSavedPlan(saved);
        mutated = true;
    };
    if (reload)
    {
        initial.Reload();
        Check(initial.State.Plan.Identifier == "fixture-one");
    }
    else
    {
        initial.Dispose();
        using var restored = new LicenseManager(snapshotProvider, snapshotStorage, clock: snapshotClock.Read);
        Check(restored.InitialRestoreError is null && restored.State.Plan.Identifier == "fixture-one");
    }
    Check(mutated && !saved.AsSpan().SequenceEqual(loadedSnapshot));
    Check(ReferenceEquals(snapshotStorage.Data, saved));
    Check(snapshotStorage.LoadCount == loads + 1 && snapshotStorage.SaveCount == saves);
    Check(snapshotProvider.ActivationCount == 0 && snapshotProvider.ValidationCount == 0 && snapshotProvider.ReleaseCount == 0);
}
Console.WriteLine(".NET deferred completion, clock recovery, exclusive resume and owned restore snapshots passed.");

sealed class CompletionClock
{
    public string? Fault;
    public string? FaultOnNextRead;
    public Action? BeforeRead;
    public DateTimeOffset Read()
    {
        var before = BeforeRead;
        BeforeRead = null;
        before?.Invoke();
        var fault = Fault;
        if (FaultOnNextRead is { } next) { Fault = next; FaultOnNextRead = null; }
        return fault switch
        {
            "throw" => throw new InvalidOperationException("Custom clock failed."),
            "range" => DateTimeOffset.MaxValue,
            _ => DateTimeOffset.FromUnixTimeSeconds(2000),
        };
    }
}

sealed class MemoryStorage : ILicenseStorage
{
    public byte[]? Data;
    public bool Fails;
    public int SaveCount;
    public int LoadCount;
    public int? FailOnSave;
    public Action? BeforeSave;
    public Action? AfterSave;
    public Action? BeforeLoad;
    public byte[]? Load()
    {
        LoadCount++;
        var data = Data;
        var before = BeforeLoad;
        BeforeLoad = null;
        before?.Invoke();
        return data;
    }
    public void Save(byte[] record)
    {
        SaveCount++;
        try
        {
            if (Fails || FailOnSave == SaveCount) throw new IOException();
            var before = BeforeSave;
            BeforeSave = null;
            before?.Invoke();
            Data = record;
        }
        finally
        {
            var after = AfterSave;
            AfterSave = null;
            after?.Invoke();
        }
    }
}
sealed class ReleaseProvider : ILicenseProvider
{
    public int ActivationCount;
    public LicenseActivationRequest? LastActivationRequest;
    public int ValidationCount;
    public int ReleaseCount;
    public Action? BeforeActivate;
    public Action? BeforeRelease;
    public Action? BeforeValidate;
    public Func<LicenseActivation>? ActivationResponse;
    public TaskCompletionSource<LicenseActivation>? PendingActivation;
    public Exception? ActivationFailure;
    public LicenseFailure? ReleaseFailure;
    public LicenseFailure? ValidationFailure;
    public LicenseValidationResult? ValidationResult;
    // Deliberately return success even if a callback cancels the supplied token.
    public Task<LicenseActivation> ActivateAsync(LicenseActivationRequest request, CancellationToken token = default)
    {
        ActivationCount++;
        LastActivationRequest = request;
        BeforeActivate?.Invoke();
        if (ActivationFailure is { } error) return Task.FromException<LicenseActivation>(error);
        if (PendingActivation is { } pending) return pending.Task;
        if (ActivationResponse is { } response) return Task.FromResult(response());
        return new Provider(false).ActivateAsync(request, token);
    }
    public Task<LicenseValidationResult> ValidateAsync(LicenseActivation activation, CancellationToken token = default)
    {
        ValidationCount++;
        BeforeValidate?.Invoke();
        if (ValidationResult is { } result) return Task.FromResult(result);
        if (ValidationFailure is { } failure) return Task.FromException<LicenseValidationResult>(new LicenseException(failure));
        return new Provider(false).ValidateAsync(activation, token);
    }
    public Task DeactivateAsync(LicenseActivation activation, CancellationToken token = default)
    {
        ReleaseCount++;
        BeforeRelease?.Invoke();
        if (ReleaseFailure is { } failure) return Task.FromException(new LicenseException(failure));
        return Task.CompletedTask;
    }
}
sealed class Provider(bool failsDeactivation) : ILicenseProvider
{
    public Task<LicenseActivation> ActivateAsync(LicenseActivationRequest request, CancellationToken token = default)
        => Task.FromResult(new LicenseActivation("test", "paid", DateTimeOffset.FromUnixTimeSeconds(1000), "TEST-KEY", "instance"));
    public Task<LicenseValidationResult> ValidateAsync(LicenseActivation activation, CancellationToken token = default)
        => Task.FromException<LicenseValidationResult>(new LicenseException(LicenseErrorCode.TransportFailure));
    public Task DeactivateAsync(LicenseActivation activation, CancellationToken token = default)
        => failsDeactivation ? Task.FromException(new LicenseException(LicenseErrorCode.TransportFailure)) : Task.CompletedTask;
}
sealed class ValidationProvider : ILicenseProvider
{
    public LicenseValidationResult? Result;
    public int Validations;
    public Task<LicenseActivation> ActivateAsync(LicenseActivationRequest request, CancellationToken token = default)
        => new Provider(false).ActivateAsync(request, token);
    public Task<LicenseValidationResult> ValidateAsync(LicenseActivation activation, CancellationToken token = default)
    {
        Validations++;
        return Result is { } result ? Task.FromResult(result)
            : Task.FromException<LicenseValidationResult>(new LicenseException(LicenseErrorCode.TransportFailure));
    }
    public Task DeactivateAsync(LicenseActivation activation, CancellationToken token = default) => Task.CompletedTask;
}
sealed class PausedProvider : ILicenseProvider
{
    public TaskCompletionSource<LicenseValidationResult> Validation { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
    public Task<LicenseActivation> ActivateAsync(LicenseActivationRequest request, CancellationToken token = default)
        => Task.FromException<LicenseActivation>(new LicenseException(LicenseErrorCode.RequestFailure));
    public Task<LicenseValidationResult> ValidateAsync(LicenseActivation activation, CancellationToken token = default) => Validation.Task;
    public Task DeactivateAsync(LicenseActivation activation, CancellationToken token = default) => Task.CompletedTask;
}
