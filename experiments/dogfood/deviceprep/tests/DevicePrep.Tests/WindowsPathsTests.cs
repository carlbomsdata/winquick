using System.IO;
using DevicePrep;
using Xunit;

public class WindowsPathsTests
{
    [Fact]
    public void ManifestPathUsesWindowsSeparators()
    {
        var full = WindowsPaths.FromManifest(@"C:\DevicePrep", "tools/agent/agent.exe");
        Assert.Equal(@"C:\DevicePrep\tools\agent\agent.exe", full);
    }

    [Fact]
    public void ManifestPathIsUsableByTheFilesystem()
    {
        var root = Path.Combine(Path.GetTempPath(), "deviceprep-manifest");
        Directory.CreateDirectory(Path.Combine(root, "tools", "agent"));
        var full = WindowsPaths.FromManifest(root, "tools/agent/agent.exe");
        File.WriteAllText(full, "x");
        Assert.True(File.Exists(full));
        Assert.Equal(Path.Combine(root, "tools", "agent", "agent.exe"), full);
    }

    [Fact]
    public void PathComparisonIsCaseInsensitiveOnWindows()
    {
        Assert.True(WindowsPaths.SamePath(@"C:\Windows\System32", @"C:\WINDOWS\SYSTEM32"));
    }
}
