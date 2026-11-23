using System.Text.Json;
using System.Runtime.CompilerServices;
using System.Threading.Channels;

namespace NaviApps.SoftwareLicensing;

/// <summary>Executes Rust-issued effects. One manager exclusively owns a storage namespace.</summary>
/// <remarks>Already-cancelled async calls leave recovery untouched. Accepted activation and
/// release outcomes still become durable if cancellation arrives during their provider call.</remarks>
public sealed class LicenseManager : IDisposable
{
    private readonly object gate = new();
    private SessionHandle session = new();
    private readonly ILicenseProvider provider;
    private readonly ILicenseStorage storage;
    private readonly LicenseRefreshPolicy policy;
    private readonly Func<DateTimeOffset> clock;
    private bool restorationBlocked;
    private bool restoring;
    private bool disposed;
    private bool resumingCompletion;
    private readonly List<Channel<LicenseState>> observers = [];
    private LicenseState? lastSessionState;
    private LicenseState? lastObservedState;
    private static readonly Lazy<LicenseState> UnavailableState = new(() =>
    {
        using var empty = new SessionHandle();
        return RestoreEmptySession(empty);
    });
    public LicenseFailure? InitialRestoreError { get; private set; }

    public LicenseManager(ILicenseProvider provider, ILicenseStorage storage, LicenseRefreshPolicy? refreshPolicy = null,
        bool restorePersistedActivation = true, Func<DateTimeOffset>? clock = null)
    {
        this.provider = provider;
        this.storage = storage;
        this.policy = refreshPolicy ?? LicenseRefreshPolicy.Default;
        this.clock = clock ?? (() => DateTimeOffset.UtcNow);
        try { Restore(restorePersistedActivation); }
        catch (Exception e) when (e is LicenseException or JsonException or IOException or UnauthorizedAccessException)
        {
            InitialRestoreError = (e as LicenseException)?.Failure ?? new(LicenseErrorCode.StorageFailure);
            restorationBlocked = true;
            session.Dispose();
            session = new();
            RestoreEmptySession(session);
        }
    }
    private double Now => (clock() - DateTimeOffset.UnixEpoch).TotalSeconds;

    /// <summary>A credential-free snapshot. An unavailable clock yields an unlicensed state.</summary>
    public LicenseState State
    {
        get
        {
            var now = ReadObservationTime();
            lock (gate) { return ReadObservedState(now); }
        }
    }

    /// <summary>The current time-evaluated license plan.</summary>
    public LicensePlan Plan => State.Plan;

    /// <summary>Evaluates refresh eligibility using the manager's clock.</summary>
    public bool NeedsRefresh()
    {
        lock (gate) { if (restorationBlocked) return true; }
        return NeedsRefreshAt(ReadObservationTime());
    }

    /// <summary>Evaluates refresh eligibility at the supplied observed time.</summary>
    /// <remarks>Does not invoke the manager's clock, provider or storage. The session records
    /// the supplied time for rollback protection. Unsupported times conservatively return true.</remarks>
    public bool NeedsRefresh(DateTimeOffset now) => NeedsRefreshAt((now - DateTimeOffset.UnixEpoch).TotalSeconds);

    private bool NeedsRefreshAt(double? time)
    {
        lock (gate)
        {
            if (restorationBlocked) return true;
            ObjectDisposedException.ThrowIf(disposed, this);
            if (time is not { } now) return true;
            try { return Read(SendAt("needsRefresh", now)).NeedsRefresh; }
            catch (LicenseException error) when (error.Failure.Code == LicenseErrorCode.InvalidData)
            {
                return true;
            }
        }
    }

    private double? ReadObservationTime()
    {
        try { return Now; }
        catch { return null; }
    }

    // Caller holds gate. Public fallback must not replace Rust's operation flags.
    private LicenseState ReadObservedState(double? time)
    {
        ObjectDisposedException.ThrowIf(disposed, this);
        if (time is { } now)
        {
            try { return Read(SendAt("snapshot", now)).State; }
            catch (LicenseException error) when (error.Failure.Code == LicenseErrorCode.InvalidData) { }
        }
        var unavailable = UnavailableState.Value;
        PublishObserved(unavailable);
        return unavailable;
    }

    /// <summary>Latest credential-free snapshots. Cancel iteration to unsubscribe; no callbacks run under a lock.</summary>
    public async IAsyncEnumerable<LicenseState> WatchStatesAsync([EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        var channel = Channel.CreateBounded<LicenseState>(new BoundedChannelOptions(1)
        {
            FullMode = BoundedChannelFullMode.DropOldest,
            SingleReader = true,
            AllowSynchronousContinuations = false,
        });
        var now = ReadObservationTime();
        lock (gate)
        {
            var state = ReadObservedState(now);
            observers.Add(channel);
            channel.Writer.TryWrite(state);
        }
        try
        {
            await foreach (var state in channel.Reader.ReadAllAsync(cancellationToken).ConfigureAwait(false)) yield return state;
        }
        finally
        {
            lock (gate) { observers.Remove(channel); channel.Writer.TryComplete(); }
        }
    }

    public async Task<LicenseState> ActivateAsync(LicenseActivationRequest request, CancellationToken cancellationToken = default)
    {
        await RecoverAsync(cancellationToken).ConfigureAwait(false);
        cancellationToken.ThrowIfCancellationRequested();
        var start = Send("activate", ("licenseKey", request.LicenseKey));
        return (await DriveAsync(start, cancellationToken).ConfigureAwait(false)).State;
    }
    /// <summary>Persists an activation whose authority, product scope and current entitlement the host has verified.</summary>
    /// <remarks>No provider check runs here. Well-formed values alone do not establish entitlement.
    /// A pending activation can only be resolved with the key from its saved request.</remarks>
    public LicenseState AdoptExternallyVerifiedActivation(LicenseActivation activation)
    {
        RequireRestored();
        return Read(Persist(Send("adoptExternallyVerifiedActivation", ("activation", activation)))).State;
    }

    /// <summary>Clears pending activation after authoritative external proof that the saved request did not apply.</summary>
    /// <remarks>Only persistence is driven here. Cancellation, timeout or a missing local activation is not proof.
    /// The host must obtain genuine external authority before calling this method.</remarks>
    public LicenseState ConfirmActivationNotAppliedAfterExternalVerification()
    {
        RequireRestored();
        return Read(Persist(Send("confirmActivationNotAppliedAfterExternalVerification"))).State;
    }

    /// <summary>Clears pending deactivation after authoritative external proof that the saved instance was released.</summary>
    /// <remarks>Only persistence is driven here. No provider check runs; the host must obtain genuine
    /// external authority confirming release before calling this method.</remarks>
    public LicenseState ConfirmDeactivationAfterExternalVerification()
    {
        RequireRestored();
        return Read(Persist(Send("confirmDeactivationAfterExternalVerification"))).State;
    }

    /// <summary>Refreshes manually by default; onlyIfDue lets Rust honor its persisted refresh deadline.</summary>
    public async Task<LicenseRefreshResult> RefreshAsync(bool onlyIfDue = false, CancellationToken cancellationToken = default)
    {
        await RecoverAsync(cancellationToken).ConfigureAwait(false);
        cancellationToken.ThrowIfCancellationRequested();
        var start = Send("refresh", ("onlyIfDue", onlyIfDue));
        var reply = await DriveAsync(start, cancellationToken).ConfigureAwait(false);
        return new(reply.Outcome ?? throw new LicenseException(LicenseErrorCode.UnexpectedProviderResponse), reply.State);
    }
    public async Task<LicenseState> DeactivateAsync(CancellationToken cancellationToken = default)
    {
        await RecoverAsync(cancellationToken).ConfigureAwait(false);
        cancellationToken.ThrowIfCancellationRequested();
        var start = Send("deactivate");
        return (await DriveAsync(start, cancellationToken).ConfigureAwait(false)).State;
    }
    /// <summary>Retries a pending save, resuming a planned provider call once without repeating a completed mutation.</summary>
    public async Task RetryPersistenceAsync(CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (await ResumeCompletionAsync(cancellationToken).ConfigureAwait(false))
        {
            lock (gate) { restorationBlocked = false; InitialRestoreError = null; }
            return;
        }
        bool blocked;
        lock (gate) { blocked = restorationBlocked; }
        if (blocked && !Read(Send("snapshot")).State.HasPendingPersistence) Reload();
        cancellationToken.ThrowIfCancellationRequested();
        var start = Send("retryPersistence");
        await DriveAsync(start, cancellationToken).ConfigureAwait(false);
        lock (gate) { restorationBlocked = false; InitialRestoreError = null; }
    }
    public void Reload() => Restore(true, requireIdle: true);
    private async Task RecoverAsync(CancellationToken token)
    {
        token.ThrowIfCancellationRequested();
        await ResumeCompletionAsync(token).ConfigureAwait(false);
        token.ThrowIfCancellationRequested();
        if (Read(Send("snapshot")).State.HasPendingPersistence)
            await RetryPersistenceAsync(token).ConfigureAwait(false);
        token.ThrowIfCancellationRequested();
        bool blocked;
        lock (gate) { blocked = restorationBlocked; }
        if (blocked) Reload();
        token.ThrowIfCancellationRequested();
    }
    private void RequireRestored()
    {
        lock (gate) { if (restorationBlocked) throw new LicenseException(LicenseErrorCode.StorageFailure); }
    }
    private sealed record Reply(LicenseState State, LicenseRefreshOutcome? Outcome, LicenseFailure? Error, bool NeedsRefresh);
    private static Reply Read(JsonElement element)
    {
        Reply reply;
        try { reply = element.Deserialize<Reply>(Native.Json) ?? throw new JsonException(); }
        catch (JsonException) { throw new LicenseException(LicenseErrorCode.UnexpectedProviderResponse); }
        if (reply.Error is { } error)
        {
            if (error.Code == LicenseErrorCode.Cancelled) throw new OperationCanceledException();
            throw new LicenseException(error);
        }
        return reply;
    }
    private JsonElement Send(string command, params (string, object?)[] values)
    {
        // The host clock may re-enter or marshal to another thread.
        var now = Now;
        lock (gate) { return SendAt(command, now, values); }
    }
    // Caller holds gate; no host callbacks or synchronous observer continuations run here.
    private JsonElement SendAt(string command, double? now, params (string, object?)[] values)
    {
        RequireDispatch(allowDuringRestore: command is "snapshot" or "needsRefresh" or "completeEffect");
        var input = new Dictionary<string, object?> { ["command"] = command, ["now"] = now };
        foreach (var (name, value) in values) if (value is not null) input[name] = value;
        return Observe(Native.Call(session, input));
    }
    private void RequireDispatch(bool allowDuringRestore = false)
    {
        ObjectDisposedException.ThrowIf(disposed, this);
        // Reads and outstanding receipts still belong to the committed session.
        if (restoring && !allowDuringRestore) throw new LicenseException(LicenseErrorCode.OperationInProgress);
    }
    private JsonElement Observe(JsonElement reply)
    {
        Publish(reply.GetProperty("state").Deserialize<LicenseState>(Native.Json)!);
        return reply;
    }
    private void Publish(LicenseState state)
    {
        lastSessionState = state;
        PublishObserved(state);
    }
    private void PublishObserved(LicenseState state)
    {
        if (state != lastObservedState)
        {
            lastObservedState = state;
            foreach (var observer in observers) observer.Writer.TryWrite(state);
        }
    }
    private static LicenseState RestoreEmptySession(SessionHandle session)
        => Read(Native.Call(session, new { command = "restore", policy = LicenseRefreshPolicy.Never, now = 0 })).State;

    private void Restore(bool readStorage, bool requireIdle = false)
    {
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            if (restoring) throw new LicenseException(LicenseErrorCode.OperationInProgress);
            restoring = true;
        }
        SessionHandle? candidate = null;
        try
        {
            if (requireIdle)
            {
                var now = ReadRestoreTime();
                lock (gate) { Publish(CheckReload(now).State); }
            }
            candidate = new SessionHandle();
            byte[]? record;
            try { record = readStorage ? storage.Load() : null; }
            catch { throw new LicenseException(LicenseErrorCode.StorageFailure); }
            if (record is not null) Native.StageStoredRecord(candidate, record);
            var restoredAt = ReadRestoreTime();
            lock (gate)
            {
                // Keep admission and replacement atomic with snapshot reads,
                // which can advance the observed time in the live session.
                if (requireIdle) CheckReload(restoredAt);
                // Loading and schema upgrades have no effects.
                var reply = Read(Native.Call(candidate, new { command = "restore", policy, now = restoredAt }));
                session.Dispose();
                session = candidate;
                candidate = null;
                restorationBlocked = false;
                InitialRestoreError = null;
                Publish(reply.State);
            }
        }
        finally
        {
            candidate?.Dispose();
            lock (gate) { restoring = false; }
        }
    }

    // Caller holds gate. Rust checks the operation lease and observed time.
    private Reply CheckReload(double now) => Read(Native.Call(session, new { command = "checkReload", now }));

    private double ReadRestoreTime()
    {
        try { return Now; }
        catch { throw new LicenseException(LicenseErrorCode.InvalidData); }
    }

    private JsonElement CompleteSave(ulong id, bool success)
    {
        // A reentrant clock observes an outstanding effect until this receipt is submitted.
        var now = ReadObservationTime();
        lock (gate) { return SendAt("completeEffect", now, ("id", id), ("completion", new { kind = "persisted", success })); }
    }
    private JsonElement CompleteProvider(ulong id, object result)
    {
        var now = ReadObservationTime();
        lock (gate)
        {
            RequireDispatch(allowDuringRestore: true);
            return Observe(Native.CompleteProvider(session, id, now, result));
        }
    }
    private async Task<bool> ResumeCompletionAsync(CancellationToken token)
    {
        token.ThrowIfCancellationRequested();
        lock (gate)
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            if (resumingCompletion) throw new LicenseException(LicenseErrorCode.OperationInProgress);
            if (Native.HasCompletion(session) == 0) return false;
            resumingCompletion = true;
        }
        try
        {
            token.ThrowIfCancellationRequested();
            var now = ReadObservationTime();
            JsonElement reply;
            lock (gate) { reply = SendAt("resumeCompletion", now, ("cancelled", token.IsCancellationRequested)); }
            await DriveAsync(reply, token).ConfigureAwait(false);
            return true;
        }
        finally { lock (gate) { resumingCompletion = false; } }
    }
    private JsonElement Persist(JsonElement reply)
    {
        while (reply.TryGetProperty("effect", out var effect) && effect.ValueKind != JsonValueKind.Null && effect.GetProperty("kind").GetString() == "persist")
        {
            var id = effect.GetProperty("id").GetUInt64();
            var success = true;
            try { storage.Save(JsonSerializer.SerializeToUtf8Bytes(effect.GetProperty("record"))); }
            catch { success = false; }
            reply = CompleteSave(id, success);
        }
        return reply;
    }
    private async Task<Reply> DriveAsync(JsonElement reply, CancellationToken token)
    {
        while (true)
        {
            // Rust's outstanding receipt rejects reentrant retries while Save runs.
            reply = Persist(reply);
            var state = Read(reply);
            if (!reply.TryGetProperty("effect", out var effect) || effect.ValueKind == JsonValueKind.Null) return state;
            var id = effect.GetProperty("id").GetUInt64();
            var operation = effect.GetProperty("operation").GetString();
            object result;
            var providerEntered = false;
            try
            {
                token.ThrowIfCancellationRequested();
                switch (operation)
                {
                    case "activate":
                        var key = effect.GetProperty("licenseKey").ValueKind == JsonValueKind.Null ? null : effect.GetProperty("licenseKey").GetString();
                        providerEntered = true;
                        var activation = await provider.ActivateAsync(new(key), token).ConfigureAwait(false);
                        result = new { kind = "activated", activation };
                        break;
                    case "validate":
                        var validationActivation = effect.GetProperty("activation").Deserialize<LicenseActivation>(Native.Json)!;
                        providerEntered = true;
                        var validation = await provider.ValidateAsync(validationActivation, token).ConfigureAwait(false);
                        token.ThrowIfCancellationRequested();
                        result = new { kind = "validated", validation };
                        break;
                    case "deactivate":
                        var releaseActivation = effect.GetProperty("activation").Deserialize<LicenseActivation>(Native.Json)!;
                        providerEntered = true;
                        await provider.DeactivateAsync(releaseActivation, token).ConfigureAwait(false);
                        result = new { kind = "deactivated" };
                        break;
                    default: throw new LicenseException(LicenseErrorCode.UnexpectedProviderResponse);
                }
            }
            catch (Exception e) when (e is OperationCanceledException ||
                (operation == "validate" && token.IsCancellationRequested))
            {
                result = new
                {
                    kind = "failed",
                    failure = new LicenseFailure(LicenseErrorCode.Cancelled,
                    MutationOutcome: providerEntered ? null : LicenseMutationOutcome.NotApplied)
                };
            }
            catch (Exception e) { result = new { kind = "failed", failure = (e as LicenseException)?.Failure ?? new(LicenseErrorCode.TransportFailure) }; }
            reply = CompleteProvider(id, result);
        }
    }
    public void Dispose()
    {
        lock (gate)
        {
            if (disposed) return;
            if (restoring) throw new LicenseException(LicenseErrorCode.OperationInProgress);
            // Operation flags do not depend on time. Every session transition
            // publishes them under this gate; fallback restore is idle with no cache.
            if (lastSessionState is { } state)
            {
                if (state.HasPendingPersistence) throw new LicenseException(LicenseErrorCode.PersistencePending);
                if (state.IsActivating || state.IsRefreshing || state.IsDeactivating) throw new LicenseException(LicenseErrorCode.OperationInProgress);
            }
            session.Dispose();
            foreach (var observer in observers) observer.Writer.TryComplete();
            observers.Clear();
            disposed = true;
        }
    }
}
