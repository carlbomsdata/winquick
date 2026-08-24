using System;
using System.Runtime.InteropServices;
using System.Text;

namespace DevicePrep;

/// <summary>Thin wrappers over the Win32 APIs the provisioning report needs.</summary>
public static class MachineInfo
{
    [DllImport("kernel32.dll", CharSet = CharSet.Ansi, SetLastError = true)]
    private static extern uint GetSystemDirectoryW(StringBuilder buffer, uint size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern ulong GetTickCount64();

    /// <summary>Full path of the Windows system directory, e.g. C:\Windows\System32.</summary>
    public static string SystemDirectory()
    {
        var sb = new StringBuilder(260);
        uint n = GetSystemDirectoryW(sb, (uint)sb.Capacity);
        if (n == 0) throw new InvalidOperationException("GetSystemDirectory failed");
        return sb.ToString();
    }

    /// <summary>Milliseconds since the machine booted.</summary>
    public static TimeSpan Uptime() => TimeSpan.FromMilliseconds(GetTickCount64());
}
