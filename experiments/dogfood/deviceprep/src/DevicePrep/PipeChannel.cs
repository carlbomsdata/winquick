using System;
using System.IO.Pipes;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace DevicePrep;

/// <summary>
/// Local control channel between the provisioning UI and the worker service.
/// Windows named pipes are used so no TCP port has to be opened.
/// </summary>
public static class PipeChannel
{
    public const string PipeName = @"\\.\pipe\deviceprep-control";

    /// <summary>Serve exactly one request, uppercasing whatever it receives.</summary>
    public static async Task<string> RoundTripAsync(string request, CancellationToken ct = default)
    {
        await using var server = new NamedPipeServerStream(
            PipeName, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);

        var serve = Task.Run(async () =>
        {
            await server.WaitForConnectionAsync(ct);
            var buf = new byte[256];
            int n = await server.ReadAsync(buf, 0, buf.Length, ct);
            var text = Encoding.UTF8.GetString(buf, 0, n).ToUpperInvariant();
            var outBytes = Encoding.UTF8.GetBytes(text);
            await server.WriteAsync(outBytes, 0, outBytes.Length, ct);
            await server.FlushAsync(ct);
        }, ct);

        await using var client = new NamedPipeClientStream(
            ".", PipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
        await client.ConnectAsync(5000, ct);
        var req = Encoding.UTF8.GetBytes(request);
        await client.WriteAsync(req, 0, req.Length, ct);
        await client.FlushAsync(ct);

        var rbuf = new byte[256];
        int rn = await client.ReadAsync(rbuf, 0, rbuf.Length, ct);
        await serve;
        return Encoding.UTF8.GetString(rbuf, 0, rn);
    }
}
