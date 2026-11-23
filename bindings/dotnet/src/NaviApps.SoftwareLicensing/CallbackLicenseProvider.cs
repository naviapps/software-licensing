namespace NaviApps.SoftwareLicensing;

/// <summary>Adapts a host-owned, read-only access check to the license lifecycle.</summary>
/// <remarks>The callback must never create or release remote activations.</remarks>
public sealed class CallbackLicenseProvider : ILicenseProvider
{
    public sealed class Configuration
    {
        public string Source { get; }
        public string PlanIdentifier { get; }

        /// <summary>Validates and normalizes the host's source and plan in Rust.</summary>
        public Configuration(string source, string planIdentifier)
        {
            LicenseActivation value;
            try
            {
                value = LicenseActivation.CreateValidated(source, planIdentifier, DateTimeOffset.UnixEpoch);
            }
            catch (LicenseException error) when (error.Failure.Code == LicenseErrorCode.InvalidData)
            {
                throw new LicenseException(LicenseErrorCode.InvalidProviderConfiguration);
            }
            Source = value.Source;
            PlanIdentifier = value.PlanIdentifier;
        }
    }

    private readonly Configuration configuration;
    private readonly Func<CancellationToken, Task<bool>> checkAccess;
    private readonly Func<DateTimeOffset> clock;

    public CallbackLicenseProvider(Configuration configuration, Func<CancellationToken, Task<bool>> checkAccess,
        Func<DateTimeOffset>? clock = null)
    {
        ArgumentNullException.ThrowIfNull(configuration);
        ArgumentNullException.ThrowIfNull(checkAccess);
        this.configuration = configuration;
        this.checkAccess = checkAccess;
        this.clock = clock ?? (() => DateTimeOffset.UtcNow);
    }

    public async Task<LicenseActivation> ActivateAsync(LicenseActivationRequest request, CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(request);
        var reply = await PerformAsync("activate", null, request.LicenseKey, cancellationToken).ConfigureAwait(false);
        return reply.Activation ?? throw NotApplied(LicenseErrorCode.UnexpectedProviderResponse);
    }

    public async Task<LicenseValidationResult> ValidateAsync(LicenseActivation activation, CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(activation);
        var reply = await PerformAsync("validate", activation, null, cancellationToken).ConfigureAwait(false);
        return reply.Validation ?? throw NotApplied(LicenseErrorCode.UnexpectedProviderResponse);
    }

    public async Task DeactivateAsync(LicenseActivation activation, CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(activation);
        await PerformAsync("deactivate", activation, null, cancellationToken).ConfigureAwait(false);
    }

    private async Task<ProviderResult> PerformAsync(string operation, LicenseActivation? activation, string? licenseKey, CancellationToken token)
    {
        AccessDecision Decide(bool? hasAccess = null)
            => Native.MapAccess(new(configuration.Source, configuration.PlanIdentifier, operation,
                activation, licenseKey, hasAccess, clock()));

        AccessDecision decision;
        try
        {
            token.ThrowIfCancellationRequested();
            decision = Decide();
            if (decision.NeedsAccessCheck)
            {
                token.ThrowIfCancellationRequested();
                var hasAccess = await checkAccess(token).ConfigureAwait(false);
                token.ThrowIfCancellationRequested();
                decision = Decide(hasAccess);
            }
            token.ThrowIfCancellationRequested();
        }
        catch (LicenseException error)
        {
            throw new LicenseException(error.Failure with { MutationOutcome = LicenseMutationOutcome.NotApplied });
        }
        catch (Exception error)
        {
            var code = error is OperationCanceledException || token.IsCancellationRequested
                ? LicenseErrorCode.Cancelled : LicenseErrorCode.TransportFailure;
            throw NotApplied(code);
        }
        var reply = decision.Reply ?? throw NotApplied(LicenseErrorCode.UnexpectedProviderResponse);
        if (reply.Failure is { } failure) throw new LicenseException(failure);
        return reply;
    }

    private static LicenseException NotApplied(LicenseErrorCode code)
        => new(new LicenseFailure(code, MutationOutcome: LicenseMutationOutcome.NotApplied));
}
