using System.Runtime.Versioning;
using System.Text.Json;

namespace NaviApps.SoftwareLicensing;

/// <summary>User-scoped DPAPI and atomic file replacement, implemented in Rust. Each private absolute directory owns one license.dpapi record.</summary>
/// <remarks>The namespace supplies encryption entropy only; it does not select a file or partition the directory.
/// Use a distinct directory for each logical record/manager namespace, and keep both directory and namespace stable.</remarks>
[SupportedOSPlatform("windows10.0.26100")]
public sealed class WindowsLicenseStorage(string directory, string @namespace) : ILicenseStorage
{
    public byte[]? Load()
    {
        var record = Native.Storage(new { operation = "load", directory, @namespace });
        return record.ValueKind == JsonValueKind.Null ? null : JsonSerializer.SerializeToUtf8Bytes(record);
    }
    public void Save(byte[] record)
    {
        using var document = JsonDocument.Parse(record);
        Native.Storage(new { operation = "save", directory, @namespace, record = document.RootElement });
    }
    public void Delete() => Native.Storage(new { operation = "delete", directory, @namespace });
}
