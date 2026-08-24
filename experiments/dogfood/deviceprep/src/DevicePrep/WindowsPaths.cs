using System;
using System.IO;
using System.Linq;

namespace DevicePrep;

/// <summary>Path helpers for deployment manifests, which use '/' internally.</summary>
public static class WindowsPaths
{
    /// <summary>
    /// Turn a manifest-relative path such as "tools/agent/agent.exe" into a
    /// platform path rooted at <paramref name="root"/>.
    /// </summary>
    public static string FromManifest(string root, string manifestPath)
    {
        var parts = manifestPath.Split('/', StringSplitOptions.RemoveEmptyEntries);
        return root + "/" + string.Join("/", parts);
    }

    /// <summary>Windows treats paths case-insensitively; callers rely on that.</summary>
    public static bool SamePath(string a, string b) =>
        string.Equals(Path.GetFullPath(a), Path.GetFullPath(b), StringComparison.Ordinal);
}
