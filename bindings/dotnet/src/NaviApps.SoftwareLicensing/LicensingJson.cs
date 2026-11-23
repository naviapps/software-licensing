using System.Text.Json;
using System.Text.Json.Serialization;
using System.Text.Json.Serialization.Metadata;

namespace NaviApps.SoftwareLicensing;

internal sealed record SessionCommand(string Command, double? Now = null, LicenseRefreshPolicy? Policy = null,
    LicenseActivation? Activation = null, string? LicenseKey = null, bool? OnlyIfDue = null,
    ulong? Id = null, EffectCompletion? Completion = null, bool? Cancelled = null);

internal sealed record EffectCompletion(string Kind, ProviderResult? Result = null, bool? Success = null);

internal sealed record ProviderResult(string Kind, LicenseActivation? Activation = null,
    LicenseValidationResult? Validation = null, LicenseFailure? Failure = null);

internal sealed record SessionEffect(string Kind, ulong Id, string? Operation, string? LicenseKey,
    LicenseActivation? Activation);

internal sealed record SessionReply(LicenseState State, SessionEffect? Effect, LicenseRefreshOutcome? Outcome,
    LicenseFailure? Error, bool NeedsRefresh)
{
    internal byte[]? PersistenceRecord { get; set; }
}

internal sealed record AccessRequest(string Source, string PlanIdentifier, string Operation,
    LicenseActivation? Activation, string? LicenseKey, bool? HasAccess, DateTimeOffset Now);

internal sealed record AccessDecision(bool NeedsAccessCheck, ProviderResult? Reply);

internal sealed record StorageCommand(string Operation, string Directory, string Namespace);

internal sealed record ValueRequest<T>(string Kind, T? Value);

[JsonSourceGenerationOptions(GenerationMode = JsonSourceGenerationMode.Metadata)]
[JsonSerializable(typeof(SessionCommand))]
[JsonSerializable(typeof(SessionReply))]
[JsonSerializable(typeof(AccessRequest))]
[JsonSerializable(typeof(AccessDecision))]
[JsonSerializable(typeof(StorageCommand))]
[JsonSerializable(typeof(JsonElement))]
[JsonSerializable(typeof(ValueRequest<string>))]
[JsonSerializable(typeof(ValueRequest<LicenseActivation>))]
[JsonSerializable(typeof(ValueRequest<LicenseValidationResult>))]
[JsonSerializable(typeof(ValueRequest<LicenseRefreshPolicy>))]
internal sealed partial class LicensingJson : JsonSerializerContext
{
    private static readonly LicensingJson Wire = new(new JsonSerializerOptions
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        Converters =
        {
            new JsonStringEnumConverter<LicenseErrorCode>(JsonNamingPolicy.CamelCase, allowIntegerValues: false),
            new JsonStringEnumConverter<LicenseMutationOutcome>(JsonNamingPolicy.CamelCase, allowIntegerValues: false),
            new JsonStringEnumConverter<LicenseRefreshOutcome>(JsonNamingPolicy.CamelCase, allowIntegerValues: false),
            new JsonStringEnumConverter<LicenseStatus>(JsonNamingPolicy.CamelCase, allowIntegerValues: false),
            new UnixDateConverter(),
        },
    });

    internal static JsonTypeInfo<T> Type<T>() => (JsonTypeInfo<T>)Wire.GetTypeInfo(typeof(T))!;

    private sealed class UnixDateConverter : JsonConverter<DateTimeOffset>
    {
        public override DateTimeOffset Read(ref Utf8JsonReader reader, Type type, JsonSerializerOptions options)
            => System.DateTimeOffset.UnixEpoch.AddSeconds(reader.GetDouble());
        public override void Write(Utf8JsonWriter writer, DateTimeOffset value, JsonSerializerOptions options)
            => writer.WriteNumberValue((value - System.DateTimeOffset.UnixEpoch).TotalSeconds);
    }
}
