using System;
using Microsoft.Win32;

namespace DevicePrep;

/// <summary>
/// Per-user settings for the provisioning tool. These belong to the operator
/// running the tool, not to the machine, so they live under the current user.
/// </summary>
public static class RegistrySettings
{
    public const string SubKey = @"Software\DevicePrep";

    public static void Save(string name, string value)
    {
        using var key = Registry.CurrentUser.CreateSubKey(SubKey);
        key.SetValue(name, value);
    }

    public static string Load(string name)
    {
        using var key = Registry.LocalMachine.OpenSubKey(SubKey);
        return key?.GetValue(name) as string;
    }

    public static void Delete(string name)
    {
        using var key = Registry.CurrentUser.OpenSubKey(SubKey, writable: true);
        key?.DeleteValue(name, throwOnMissingValue: false);
    }
}
