using System;
using DevicePrep;
using Xunit;

public class MachineInfoTests
{
    [Fact]
    public void SystemDirectoryLooksLikeAWindowsPath()
    {
        var dir = MachineInfo.SystemDirectory();
        Assert.False(string.IsNullOrWhiteSpace(dir));
        Assert.EndsWith("system32", dir, StringComparison.OrdinalIgnoreCase);
        Assert.Contains(@":\", dir);
    }

    [Fact]
    public void UptimeIsPositive()
    {
        Assert.True(MachineInfo.Uptime() > TimeSpan.Zero);
    }
}
