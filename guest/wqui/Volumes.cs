using System;
using System.Diagnostics;
using System.IO;
using System.Text;

namespace WinQuick.Ui;

/// <summary>
/// Finding the volumes a session is given, and refreshing the guest's view of
/// one when the host has rewritten it underneath.
///
/// Drive letters are not stable, so volumes are found by a marker file rather
/// than assumed. The refresh matters because of how a session starts: the guest
/// is restored from a snapshot taken with a different application volume
/// attached, so Windows is holding a cached directory for a volume whose
/// contents have since been replaced. Dismounting and remounting is what makes
/// it read the disk again — the same trick the guest agent uses on the mailbox.
/// </summary>
public static class Volumes
{
    /// <summary>Marker identifying the volume carrying wqui.exe.</summary>
    public const string BridgeMarker = "WQDESK.TXT";
    /// <summary>Marker identifying the volume carrying the application.</summary>
    public const string AppMarker = "WQAPP.TXT";

    static string _app;

    /// <summary>The drive holding the application volume, e.g. "E:".</summary>
    public static string App => _app ??= Find(AppMarker)
        ?? throw new InvalidOperationException(
            "no application volume is attached to this session");

    public static string Find(string marker)
    {
        foreach (char c in "DEFGHIJKLMNOP")
        {
            string drive = c + ":";
            try
            {
                if (File.Exists(drive + "\\" + marker)) return drive;
            }
            catch { /* an unreadable drive is simply not the one */ }
        }
        return null;
    }

    /// <summary>
    /// Make the guest re-read the application volume from disk.
    /// </summary>
    public static string RemountApp()
    {
        string drive = Find(AppMarker)
            ?? throw new InvalidOperationException("no application volume is attached");

        // The volume GUID is stable for the life of the filesystem; the mount
        // point is what has to be torn down and rebuilt.
        string guid = Run("mountvol", $"{drive} /L").Trim();
        if (string.IsNullOrWhiteSpace(guid) || !guid.StartsWith("\\\\?\\"))
            throw new InvalidOperationException($"could not read the volume id of {drive}: {guid}");

        Run("mountvol", $"{drive} /P");
        Run("mountvol", $"{drive} {guid}");

        // Prove it came back, rather than reporting success into the void.
        for (int i = 0; i < 100 && !File.Exists(drive + "\\" + AppMarker); i++)
            System.Threading.Thread.Sleep(50);
        if (!File.Exists(drive + "\\" + AppMarker))
            throw new InvalidOperationException($"{drive} did not come back after remounting");

        _app = drive;
        return drive;
    }

    static string Run(string exe, string args)
    {
        var psi = new ProcessStartInfo(exe, args)
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            CreateNoWindow = true,
        };
        using var p = Process.Start(psi);
        string output = p.StandardOutput.ReadToEnd() + p.StandardError.ReadToEnd();
        p.WaitForExit(30000);
        return output;
    }

    /// <summary>
    /// Resolve a program path given to `launch`.
    ///
    /// A relative path is taken against the application volume, so
    /// `launch app\MyApp.exe` means what it looks like it means wherever the
    /// volume happened to land.
    /// </summary>
    public static string ResolveProgram(string program)
    {
        if (Path.IsPathRooted(program)) return program;
        string candidate = Path.Combine(App + "\\", program);
        return File.Exists(candidate) ? candidate : program;
    }
}
