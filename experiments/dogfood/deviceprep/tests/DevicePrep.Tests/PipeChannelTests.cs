using System.Threading.Tasks;
using DevicePrep;
using Xunit;

public class PipeChannelTests
{
    [Fact]
    public async Task ControlChannelRoundTripsARequest()
    {
        var reply = await PipeChannel.RoundTripAsync("provision");
        Assert.Equal("PROVISION", reply);
    }
}
