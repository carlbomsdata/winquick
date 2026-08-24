using System;
using System.IO;
using System.Text;

namespace DevicePrep;

/// <summary>Writes the diagnostic report the support team asks operators for.</summary>
public static class ProvisioningReport
{
    public static string Write(string directory)
    {
        Directory.CreateDirectory(directory);
        var path = Path.Combine(directory, "provisioning-report.txt");
        var sb = new StringBuilder();
        sb.AppendLine($"machine        : {Environment.MachineName}");
        sb.AppendLine($"os             : {Environment.OSVersion}");
        sb.AppendLine($"system dir     : {MachineInfo.SystemDirectory()}");
        sb.AppendLine($"uptime         : {MachineInfo.Uptime():g}");
        sb.AppendLine($"temp           : {Path.GetTempPath()}");
        File.WriteAllText(path, sb.ToString());
        return path;
    }
}
