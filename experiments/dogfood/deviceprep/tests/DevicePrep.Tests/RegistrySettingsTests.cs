using System;
using DevicePrep;
using Xunit;

public class RegistrySettingsTests : IDisposable
{
    const string Name = "LastProvisionedBy";

    public void Dispose() => RegistrySettings.Delete(Name);

    [Fact]
    public void SavedValueCanBeReadBack()
    {
        RegistrySettings.Save(Name, "operator-7");
        Assert.Equal("operator-7", RegistrySettings.Load(Name));
    }

    [Fact]
    public void MissingValueReturnsNull()
    {
        RegistrySettings.Delete(Name);
        Assert.Null(RegistrySettings.Load(Name));
    }
}
