using System.IO;
using DevicePrep;
using Xunit;

public class ProvisioningReportTests
{
    [Fact]
    public void ReportContainsTheSystemDirectory()
    {
        var dir = Path.Combine(Path.GetTempPath(), "deviceprep-report");
        var path = ProvisioningReport.Write(dir);
        var text = File.ReadAllText(path);
        Assert.Contains("system dir", text);
        Assert.Contains(@":\", text);
    }
}
