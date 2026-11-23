using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

[assembly: InternalsVisibleTo("SoftwareLicensing.Tests")]

namespace NaviApps.SoftwareLicensing;

internal sealed class SessionHandle : SafeHandleZeroOrMinusOneIsInvalid
{
    public SessionHandle() : base(true) => SetHandle(Native.Create());
    protected override bool ReleaseHandle() { Native.Destroy(handle); return true; }
}

internal static class Native
{
    private const string Library = "software_licensing";
    private const int MaximumJsonBytes = 1_048_576;
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);
    internal static void RequireAbi(uint version)
    {
        if (version != 6) throw new LicenseException(LicenseErrorCode.IncompatibleAbiVersion);
    }
    private static void CheckAbi()
    {
        try { RequireAbi(AbiVersion()); }
        catch (DllNotFoundException) { throw new LicenseException(LicenseErrorCode.UnsupportedPlatform); }
        catch (Exception error) when (error is EntryPointNotFoundException or BadImageFormatException)
        { throw new LicenseException(LicenseErrorCode.IncompatibleAbiVersion); }
    }
    internal static IntPtr Create() { CheckAbi(); return SessionCreate(); }
    [StructLayout(LayoutKind.Sequential)]
    private readonly struct View { public readonly uint Code; public readonly IntPtr Data; public readonly nuint Length; }
    [DllImport(Library, EntryPoint = "software_licensing_abi_version", CallingConvention = CallingConvention.Cdecl)]
    private static extern uint AbiVersion();
    [DllImport(Library, EntryPoint = "software_licensing_session_create", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr SessionCreate();
    [DllImport(Library, EntryPoint = "software_licensing_session_destroy", CallingConvention = CallingConvention.Cdecl)]
    internal static extern void Destroy(IntPtr handle);
    [DllImport(Library, EntryPoint = "software_licensing_value", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr Value(byte[] data, nuint length);
    [DllImport(Library, EntryPoint = "software_licensing_access_reply", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr AccessReply(byte[] data, nuint length);
    [DllImport(Library, EntryPoint = "software_licensing_session_dispatch", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr Dispatch(SessionHandle handle, byte[] data, nuint length);
    [DllImport(Library, EntryPoint = "software_licensing_session_has_completion", CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint HasCompletion(SessionHandle handle);
    [DllImport(Library, EntryPoint = "software_licensing_session_stage_record", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr SessionStageRecord(SessionHandle handle, byte[] record, nuint length);
    [DllImport(Library, EntryPoint = "software_licensing_windows_storage", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr WindowsStorage(byte[] data, nuint length, byte[]? record, nuint recordLength);
    [DllImport(Library, EntryPoint = "software_licensing_json_view", CallingConvention = CallingConvention.Cdecl)]
    private static extern View JsonView(IntPtr owner);
    [DllImport(Library, EntryPoint = "software_licensing_json_persistence_record", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr JsonPersistenceRecord(IntPtr owner, out nuint length);
    [DllImport(Library, EntryPoint = "software_licensing_json_destroy", CallingConvention = CallingConvention.Cdecl)]
    private static extern void JsonDestroy(IntPtr owner);

    private static unsafe T Consume<T>(IntPtr owner)
    {
        if (owner == IntPtr.Zero) throw new LicenseException(LicenseErrorCode.InvalidData);
        try
        {
            var view = JsonView(owner);
            if (view.Data == IntPtr.Zero || view.Length > MaximumJsonBytes) throw new LicenseException(LicenseErrorCode.InvalidData);
            var bytes = new ReadOnlySpan<byte>(view.Data.ToPointer(), checked((int)view.Length));
            if (view.Code != 0)
                throw new LicenseException(JsonSerializer.Deserialize(bytes, LicensingJson.Type<LicenseFailure>())!);
            var result = JsonSerializer.Deserialize(bytes, LicensingJson.Type<T>())!;
            if (result is SessionReply { Effect.Kind: "persist" } reply)
            {
                var record = JsonPersistenceRecord(owner, out var length);
                if (record == IntPtr.Zero || length == 0 || length > MaximumJsonBytes)
                    throw new LicenseException(LicenseErrorCode.UnexpectedProviderResponse);
                reply.PersistenceRecord = new ReadOnlySpan<byte>(record.ToPointer(), checked((int)length)).ToArray();
            }
            return result;
        }
        finally { JsonDestroy(owner); }
    }
    internal static SessionReply Call(SessionHandle handle, SessionCommand command)
    {
        CheckAbi();
        if (!IsWellFormedActivation(command.Activation)) throw new LicenseException(LicenseErrorCode.InvalidData);
        return Send(handle, JsonSerializer.SerializeToUtf8Bytes(command, LicensingJson.Type<SessionCommand>()));
    }

    private static SessionReply Send(SessionHandle handle, byte[] data)
    {
        try
        {
            return Consume<SessionReply>(Dispatch(handle, data, (nuint)data.Length))
                ?? throw new LicenseException(LicenseErrorCode.UnexpectedProviderResponse);
        }
        catch (JsonException) { throw new LicenseException(LicenseErrorCode.UnexpectedProviderResponse); }
        finally { CryptographicOperations.ZeroMemory(data); }
    }

    internal static SessionReply CompleteProvider(SessionHandle handle, ulong id, double? now, ProviderResult result)
    {
        CheckAbi();
        var completion = new EffectCompletion("provider", result);
        var input = new SessionCommand("completeEffect", now, Id: id, Completion: completion);
        byte[]? data = null;
        if (IsWellFormedActivation(result.Activation) && IsWellFormedText(result.Validation?.PlanIdentifier))
        {
            try { data = JsonSerializer.SerializeToUtf8Bytes(input, LicensingJson.Type<SessionCommand>()); }
            catch (JsonException) { }
        }
        if (data is null || data.Length > MaximumJsonBytes)
        {
            // Rust still needs to acknowledge the effect and release its lease.
            if (data is not null) CryptographicOperations.ZeroMemory(data);
            var invalid = input with { Completion = completion with { Result = null } };
            data = JsonSerializer.SerializeToUtf8Bytes(invalid, LicensingJson.Type<SessionCommand>());
        }
        return Send(handle, data);
    }
    internal static void StageStoredRecord(SessionHandle handle, byte[] record)
    {
        CheckAbi();
        // A custom store may retain its mutable array during this native borrow.
        var bytes = record.ToArray();
        try { _ = Consume<JsonElement>(SessionStageRecord(handle, bytes, (nuint)bytes.Length)); }
        finally { CryptographicOperations.ZeroMemory(bytes); }
    }
    internal static JsonElement Storage(StorageCommand command, byte[]? record = null)
    {
        CheckAbi();
        var data = JsonSerializer.SerializeToUtf8Bytes(command, LicensingJson.Type<StorageCommand>());
        try { return Consume<JsonElement>(WindowsStorage(data, (nuint)data.Length, record, (nuint)(record?.Length ?? 0))); }
        finally { CryptographicOperations.ZeroMemory(data); }
    }
    internal static T ReadValue<T>(string kind, T? value = default)
    {
        CheckAbi();
        byte[] data;
        try { data = JsonSerializer.SerializeToUtf8Bytes(new ValueRequest<T>(kind, value), LicensingJson.Type<ValueRequest<T>>()); }
        catch (ArgumentException) { throw new LicenseException(LicenseErrorCode.InvalidData); }
        try { return Consume<T>(Value(data, (nuint)data.Length)); }
        finally { CryptographicOperations.ZeroMemory(data); }
    }

    internal static void RequireWellFormedText(string? value)
    {
        if (!IsWellFormedText(value)) throw new LicenseException(LicenseErrorCode.InvalidData);
    }

    private static bool IsWellFormedActivation(LicenseActivation? value) => value is null ||
        (IsWellFormedText(value.Source) && IsWellFormedText(value.PlanIdentifier) &&
         IsWellFormedText(value.LicenseKey) && IsWellFormedText(value.ActivationIdentifier));

    private static bool IsWellFormedText(string? value)
    {
        if (value is null) return true;
        // The JSON encoder would replace an unpaired UTF-16 surrogate before Rust sees it.
        try { _ = StrictUtf8.GetByteCount(value); return true; }
        catch (EncoderFallbackException) { return false; }
    }

    internal static AccessDecision MapAccess(AccessRequest command)
    {
        CheckAbi();
        if (!IsWellFormedActivation(command.Activation)) throw new LicenseException(LicenseErrorCode.InvalidData);
        var data = JsonSerializer.SerializeToUtf8Bytes(command, LicensingJson.Type<AccessRequest>());
        try
        {
            return Consume<AccessDecision>(AccessReply(data, (nuint)data.Length))
                ?? throw new LicenseException(LicenseErrorCode.UnexpectedProviderResponse);
        }
        finally { CryptographicOperations.ZeroMemory(data); }
    }
}
