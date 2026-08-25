using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace WinQuick.Ui;

/// <summary>
/// The control channel a desktop session runs on: a raw disk with no filesystem.
///
/// The obvious channel is the FAT volume WinQuick already uses to hand a command
/// to a guest, and for one command per boot it is perfect. A live session is
/// different: the host writes while Windows has the volume mounted, and the two
/// FAT implementations then hold conflicting views of the same allocation
/// tables. Windows flushes its cached view on dismount, the host writes over it,
/// and the filesystem ends up genuinely corrupt — `Fat problem while decoding`
/// from anything that tries to read it afterwards.
///
/// So a session uses a disk with no partition table and no filesystem, which
/// means Windows never mounts it and never caches it. Both sides read and write
/// whole sectors at fixed offsets, payload first and header last, and a 512-byte
/// sector write is atomic at the device. There is nothing left to corrupt.
/// </summary>
public sealed class Control : IDisposable
{
    // Sector 0 identifies the disk; the host writes it before the guest boots.
    public const long IdOffset = 0;
    public const long RequestOffset = 1 << 20;    // 1 MiB
    public const long ResponseOffset = 16 << 20;  // 16 MiB
    public const int Sector = 512;
    public const int MaxPayload = 8 << 20;

    static readonly byte[] DiskMagic = Encoding.ASCII.GetBytes("WQCTLDSK");
    static readonly byte[] ReqMagic = Encoding.ASCII.GetBytes("WQCTLREQ");
    static readonly byte[] RspMagic = Encoding.ASCII.GetBytes("WQCTLRSP");

    const uint GENERIC_READ = 0x80000000, GENERIC_WRITE = 0x40000000;
    const uint FILE_SHARE_READ = 1, FILE_SHARE_WRITE = 2;
    const uint OPEN_EXISTING = 3;
    const uint FILE_FLAG_NO_BUFFERING = 0x20000000, FILE_FLAG_WRITE_THROUGH = 0x80000000;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr sec,
                                             uint disposition, uint flags, IntPtr template);

    readonly FileStream _disk;

    Control(FileStream disk) => _disk = disk;

    /// <summary>
    /// Find the control disk by the magic the host wrote to its first sector.
    ///
    /// Retried rather than attempted once: the bridge starts as soon as the
    /// shell does, and the disks are not all enumerated by then. A single scan
    /// succeeds most of the time and fails perhaps one boot in ten, which is the
    /// worst possible failure rate — often enough to matter, rare enough to look
    /// like something else. The guest agent retries finding the mailbox volume
    /// for exactly the same reason.
    /// </summary>
    public static Control Open(int timeoutMs = 60000)
    {
        var deadline = DateTime.UtcNow.AddMilliseconds(timeoutMs);
        while (true)
        {
            var found = TryOpen(out string tried);
            if (found != null) return found;
            if (DateTime.UtcNow >= deadline)
                throw new InvalidOperationException(
                    $"no WinQuick control disk found after {timeoutMs} ms; tried{tried}");
            System.Threading.Thread.Sleep(250);
        }
    }

    static Control TryOpen(out string report)
    {
        var tried = new StringBuilder();
        for (int i = 0; i < 16; i++)
        {
            string path = $@"\\.\PhysicalDrive{i}";
            SafeFileHandle h = CreateFileW(path, GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE, IntPtr.Zero, OPEN_EXISTING,
                FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH, IntPtr.Zero);
            if (h.IsInvalid) { tried.Append($" {i}:open={Marshal.GetLastWin32Error()}"); continue; }

            // bufferSize 1 turns FileStream's own buffering off. It matters
            // twice over: a cached buffer would hide new requests written while
            // we poll the same offset, and FILE_FLAG_NO_BUFFERING requires every
            // transfer to be sector-aligned, which an internal buffer breaks.
            var fs = new FileStream(h, FileAccess.ReadWrite, bufferSize: 1);
            try
            {
                var sector = ReadSector(fs, IdOffset);
                if (Matches(sector, DiskMagic)) { report = tried.ToString(); return new Control(fs); }
                tried.Append($" {i}:nomagic");
            }
            catch (Exception ex) { tried.Append($" {i}:{ex.GetType().Name}"); }
            fs.Dispose();
        }
        report = tried.ToString();
        return null;
    }

    static bool Matches(byte[] buf, byte[] magic)
    {
        for (int i = 0; i < magic.Length; i++) if (buf[i] != magic[i]) return false;
        return true;
    }

    // Unbuffered I/O demands sector-sized, sector-aligned transfers, so every
    // read and write here works in whole sectors.
    static byte[] ReadSector(FileStream fs, long offset) => ReadAt(fs, offset, Sector);

    static byte[] ReadAt(FileStream fs, long offset, int length)
    {
        int rounded = (length + Sector - 1) / Sector * Sector;
        var buf = new byte[rounded];
        fs.Seek(offset, SeekOrigin.Begin);
        int done = 0;
        while (done < rounded)
        {
            int n = fs.Read(buf, done, rounded - done);
            if (n <= 0) throw new IOException($"short read at {offset}");
            done += n;
        }
        return buf;
    }

    static void WriteAt(FileStream fs, long offset, byte[] data)
    {
        int rounded = (data.Length + Sector - 1) / Sector * Sector;
        var buf = new byte[rounded];
        Buffer.BlockCopy(data, 0, buf, 0, data.Length);
        fs.Seek(offset, SeekOrigin.Begin);
        fs.Write(buf, 0, rounded);
        fs.Flush();
    }

    /// <summary>A request: a sequence number and an argument vector.</summary>
    public readonly record struct Request(ulong Seq, string[] Argv);

    /// <summary>
    /// Wait for a request whose sequence number we have not served yet.
    /// </summary>
    public Request WaitForRequest(ulong lastSeq, int pollMs)
    {
        while (true)
        {
            var head = ReadSector(_disk, RequestOffset);
            if (Matches(head, ReqMagic))
            {
                ulong seq = BitConverter.ToUInt64(head, 8);
                int len = BitConverter.ToInt32(head, 16);
                if (seq != lastSeq && len >= 0 && len <= MaxPayload)
                {
                    // The header is written last, so a header we can see means
                    // the payload behind it is already there.
                    var body = len == 0 ? Array.Empty<byte>() : ReadAt(_disk, RequestOffset + Sector, len);
                    string json = Encoding.UTF8.GetString(body, 0, len);
                    var argv = System.Text.Json.JsonSerializer.Deserialize<string[]>(json)
                               ?? Array.Empty<string>();
                    return new Request(seq, argv);
                }
            }
            System.Threading.Thread.Sleep(pollMs);
        }
    }

    public void Respond(ulong seq, int exitCode, string payload)
    {
        var body = Encoding.UTF8.GetBytes(payload);
        if (body.Length > MaxPayload)
        {
            body = Encoding.UTF8.GetBytes(
                "{\"ok\":false,\"error\":\"response too large for the control channel\"}");
            exitCode = 1;
        }
        WriteAt(_disk, ResponseOffset + Sector, body);

        var head = new byte[Sector];
        Buffer.BlockCopy(RspMagic, 0, head, 0, RspMagic.Length);
        BitConverter.GetBytes(seq).CopyTo(head, 8);
        BitConverter.GetBytes(body.Length).CopyTo(head, 16);
        BitConverter.GetBytes(exitCode).CopyTo(head, 20);
        WriteAt(_disk, ResponseOffset, head);
    }

    public void Dispose() => _disk.Dispose();
}
