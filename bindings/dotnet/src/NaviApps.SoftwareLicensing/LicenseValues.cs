namespace NaviApps.SoftwareLicensing;

public enum LicenseErrorCode
{
    InvalidData,
    /// <summary>Unsupported persisted record schema; stored bytes remain untouched.</summary>
    UnsupportedVersion, InvalidLicenseKey, InvalidLicense, ExpiredLicense,
    ActivationLimitReached, ActivationAlreadyPresent, ActivationPending, MutationOutcomeUnknown,
    InvalidProviderConfiguration, UnexpectedProviderResponse,
    TransportFailure, RequestFailure, ServerFailure, RateLimited, StorageFailure, OperationInProgress,
    PersistencePending, DeactivationPending, ClockRollback, GracePeriodExpired, Cancelled, UnsupportedPlatform, IncompatibleAbiVersion,
}

/// <summary>NotApplied requires proof that no mutation occurred; Unknown means the server may have changed state.</summary>
public enum LicenseMutationOutcome { NotApplied, Unknown }

public sealed record LicenseFailure(LicenseErrorCode Code, int? StatusCode = null,
    ulong? RetryAfterSeconds = null, LicenseMutationOutcome? MutationOutcome = null);

/// <summary>Stable, credential-free failure. Provider response text is never an exception message.</summary>
public sealed class LicenseException(LicenseFailure failure) : Exception(failure.Code.ToString())
{
    public LicenseFailure Failure { get; } = failure;
    public LicenseException(LicenseErrorCode code) : this(new LicenseFailure(code)) { }
}

public enum LicenseStatus { Unlicensed, Active, GracePeriod, Expired, Invalid, Deactivated }

/// <summary>Validates the provider identifiers accepted by the Rust licensing model.</summary>
public static class LicenseSource
{
    /// <summary>Returns the normalized spelling or throws <see cref="LicenseException"/> for invalid input.</summary>
    public static string NormalizeIdentifier(string identifier)
    {
        Native.RequireWellFormedText(identifier);
        return Native.ReadValue<string>("identifier", identifier);
    }
}

/// <summary>Confidential provider record; never expose this in UI state or diagnostics.</summary>
public sealed record LicenseActivation(string Source, string PlanIdentifier, DateTimeOffset ActivatedAt,
    string? LicenseKey = null, string? ActivationIdentifier = null, DateTimeOffset? ExpiresAt = null)
{
    /// <summary>Validates and normalizes a provider record without running a licensing operation.</summary>
    public static LicenseActivation CreateValidated(string source, string planIdentifier, DateTimeOffset activatedAt,
        string? licenseKey = null, string? activationIdentifier = null, DateTimeOffset? expiresAt = null)
    {
        Native.RequireWellFormedText(source);
        Native.RequireWellFormedText(planIdentifier);
        Native.RequireWellFormedText(licenseKey);
        Native.RequireWellFormedText(activationIdentifier);
        return Native.ReadValue<LicenseActivation>("activation",
            new LicenseActivation(source, planIdentifier, activatedAt, licenseKey, activationIdentifier, expiresAt));
    }

    public override string ToString() => "LicenseActivation([redacted])";
}

public sealed record LicenseValidationResult(bool IsValid, string? PlanIdentifier = null, DateTimeOffset? ExpiresAt = null)
{
    /// <summary>Validates a provider result and normalizes an optional plan identifier.</summary>
    public static LicenseValidationResult CreateValidated(bool isValid, string? planIdentifier = null, DateTimeOffset? expiresAt = null)
    {
        Native.RequireWellFormedText(planIdentifier);
        return Native.ReadValue<LicenseValidationResult>("validation", new LicenseValidationResult(isValid, planIdentifier, expiresAt));
    }
}

public sealed record LicensePlan(string Identifier, bool IsLicensed, DateTimeOffset? ExpiresAt);
public sealed record LicenseRefreshPolicy(bool IsEnabled, double ValidationInterval, double FailureGracePeriod, double ServerFailureGracePeriod, double FailureRetryInterval = 60)
{
    /// <summary>Validates the policy intervals before constructing a license manager.</summary>
    public static LicenseRefreshPolicy CreateValidated(double validationInterval, double failureGracePeriod,
        double serverFailureGracePeriod, double failureRetryInterval = 60, bool isEnabled = true)
        => Native.ReadValue<LicenseRefreshPolicy>("policy", new LicenseRefreshPolicy(isEnabled,
            validationInterval, failureGracePeriod, serverFailureGracePeriod, failureRetryInterval));

    public static LicenseRefreshPolicy Default { get; } = Native.ReadValue<LicenseRefreshPolicy>("defaultPolicy");
    public static LicenseRefreshPolicy Never { get; } = new(false, 0, 0, 0);
}

/// <summary>A Rust time-evaluated snapshot, without a license key or activation identifier.</summary>
public sealed record LicenseState(LicensePlan Plan, string? Source, LicenseStatus Status,
    DateTimeOffset? LastValidatedAt, DateTimeOffset? GracePeriodExpiresAt, LicenseFailure? LastRefreshFailure,
    bool IsActivating, bool IsRefreshing, bool IsDeactivating, bool HasActivation,
    bool HasPendingPersistence, bool HasPendingDeactivation, DateTimeOffset? NextTransitionAt = null,
    DateTimeOffset? NextRefreshAt = null, bool HasPendingActivation = false, LicenseFailure? LastOperationFailure = null);

public sealed record LicenseActivationRequest(string? LicenseKey)
{
    public static LicenseActivationRequest Automatic { get; } = new((string?)null);
    public override string ToString() => "LicenseActivationRequest([redacted])";
}

/// <summary>Native effects; Rust orders operations and owns all licensing decisions.</summary>
public interface ILicenseProvider
{
    Task<LicenseActivation> ActivateAsync(LicenseActivationRequest request, CancellationToken cancellationToken = default);
    Task<LicenseValidationResult> ValidateAsync(LicenseActivation activation, CancellationToken cancellationToken = default);
    Task DeactivateAsync(LicenseActivation activation, CancellationToken cancellationToken = default);
}

/// <summary>Atomic confidential record storage, exclusively coordinated by one manager per namespace.</summary>
/// <remarks>Deactivation saves a recovery record; deletion is a separate concrete-storage operation.</remarks>
public interface ILicenseStorage
{
    byte[]? Load();
    void Save(byte[] record);
}

/// <summary>Final refresh decision after persistence, evaluated at completion time.</summary>
/// <remarks>Grace/invalid/expired may reflect a failed check; inspect State.LastRefreshFailure.</remarks>
public enum LicenseRefreshOutcome
{
    Validated, GracePeriod, Expired, Invalid, Deactivated,
    SkippedActivationInProgress, SkippedRefreshInProgress, SkippedDeactivationInProgress,
    SkippedNoActivation, SkippedRefreshDisabled, SkippedNotDue,
}

public sealed record LicenseRefreshResult(LicenseRefreshOutcome Outcome, LicenseState State);
