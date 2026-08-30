using Newtonsoft.Json;
using Xunit;

public class CacheTests
{
    // Touches the referenced library rather than merely referencing it, so a
    // cache that restored but did not actually deliver the assembly fails here
    // instead of passing quietly.
    [Fact]
    public void JsonRoundTrips()
    {
        var json = JsonConvert.SerializeObject(new { name = "winquick", ok = true });
        dynamic? back = JsonConvert.DeserializeObject(json);
        Assert.NotNull(back);
        Assert.Equal("winquick", (string)back!.name);
        Assert.True((bool)back!.ok);
    }

    [Fact]
    public void ArithmeticStillWorks()
    {
        Assert.Equal(4, 2 + 2);
    }
}
