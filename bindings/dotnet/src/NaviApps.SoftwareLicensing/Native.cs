using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
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
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);
    internal static readonly JsonSerializerOptions Json = Options();
    internal static void RequireAbi(uint version)
    {
        if (version != 4) throw new LicenseException(LicenseErrorCode.IncompatibleAbiVersion);
    }
    private static void CheckAbi()
    {
        try { RequireAbi(AbiVersion()); }
        catch (DllNotFoundException) { throw new LicenseException(LicenseErrorCode.UnsupportedPlatform); }
        catch (Exception error) when (error is EntryPointNotFoundException or BadImageFormatException)
        { throw new LicenseException(LicenseErrorCode.IncompatibleAbiVersion); }
    }
    internal static IntPtr Create() { CheckAbi(); return SessionCreate(); }
    private static JsonSerializerOptions Options()
    {
        var options = new JsonSerializerOptions { PropertyNamingPolicy = JsonNamingPolicy.CamelCase, DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull };
        options.Converters.Add(new JsonStringEnumConverter(JsonNamingPolicy.CamelCase, allowIntegerValues: false));
        options.Converters.Add(new UnixDateConverter());
        return options;
    }
    private sealed class UnixDateConverter : JsonConverter<DateTimeOffset>
    {
        public override DateTimeOffset Read(ref Utf8JsonReader reader, Type type, JsonSerializerOptions options)
            => DateTimeOffset.UnixEpoch.AddSeconds(reader.GetDouble());
        public override void Write(Utf8JsonWriter writer, DateTimeOffset value, JsonSerializerOptions options)
            => writer.WriteNumberValue((value - DateTimeOffset.UnixEpoch).TotalSeconds);
    }
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
    [DllImport(Library, EntryPoint = "software_licensing_session_stage_record", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr SessionStageRecord(SessionHandle handle, byte[] record, nuint length);
    [DllImport(Library, EntryPoint = "software_licensing_windows_storage", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr WindowsStorage(byte[] data, nuint length);
    [DllImport(Library, EntryPoint = "software_licensing_json_view", CallingConvention = CallingConvention.Cdecl)]
    private static extern View JsonView(IntPtr owner);
    [DllImport(Library, EntryPoint = "software_licensing_json_destroy", CallingConvention = CallingConvention.Cdecl)]
    private static extern void JsonDestroy(IntPtr owner);

    private static JsonElement Consume(IntPtr owner)
    {
        if (owner == IntPtr.Zero) throw new LicenseException(LicenseErrorCode.InvalidData);
        try
        {
            var view = JsonView(owner);
            if (view.Data == IntPtr.Zero || view.Length > 1048576) throw new LicenseException(LicenseErrorCode.InvalidData);
            var bytes = new byte[(int)view.Length];
            Marshal.Copy(view.Data, bytes, 0, bytes.Length);
            using var document = JsonDocument.Parse(bytes);
            if (view.Code != 0) throw new LicenseException(document.RootElement.Deserialize<LicenseFailure>(Json)!);
            return document.RootElement.Clone();
        }
        finally { JsonDestroy(owner); }
    }
    internal static JsonElement Call(SessionHandle handle, object command)
    {
        CheckAbi();
        var data = JsonSerializer.SerializeToUtf8Bytes(command, Json);
        return Consume(Dispatch(handle, data, (nuint)data.Length));
    }
    internal static void StageStoredRecord(SessionHandle handle, byte[] record)
    {
        CheckAbi();
        // A custom store may retain its mutable array during this native borrow.
        var bytes = record.ToArray();
        Consume(SessionStageRecord(handle, bytes, (nuint)bytes.Length));
    }
    internal static JsonElement Storage(object command)
    {
        CheckAbi();
        var data = JsonSerializer.SerializeToUtf8Bytes(command, Json);
        return Consume(WindowsStorage(data, (nuint)data.Length));
    }
    internal static T ReadValue<T>(string kind, object? value = null)
    {
        CheckAbi();
        byte[] data;
        try { data = JsonSerializer.SerializeToUtf8Bytes(new { kind, value }, Json); }
        catch (ArgumentException) { throw new LicenseException(LicenseErrorCode.InvalidData); }
        return Consume(Value(data, (nuint)data.Length)).Deserialize<T>(Json)!;
    }

    internal static void RequireWellFormedText(string? value)
    {
        if (value is null) return;
        // The JSON encoder would replace an unpaired UTF-16 surrogate before Rust sees it.
        try { _ = StrictUtf8.GetByteCount(value); }
        catch (EncoderFallbackException) { throw new LicenseException(LicenseErrorCode.InvalidData); }
    }

    internal static JsonElement MapAccess(object command)
    {
        CheckAbi();
        var data = JsonSerializer.SerializeToUtf8Bytes(command, Json);
        return Consume(AccessReply(data, (nuint)data.Length));
    }
}
