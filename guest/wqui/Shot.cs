using System;
using System.IO;
using System.IO.Compression;
using System.Runtime.InteropServices;

namespace WinQuick.Ui;

/// <summary>
/// Desktop and window capture.
///
/// Pixels come from GDI into a DIB section so they can be read directly, and are
/// encoded to PNG here rather than through System.Drawing. That keeps capture
/// independent of gdiplus.dll, which is an optional package in Validation OS and
/// one more thing that can be missing when a capture fails.
/// </summary>
public static class Shot
{
    [StructLayout(LayoutKind.Sequential)]
    struct BITMAPINFOHEADER
    {
        public uint biSize;
        public int biWidth, biHeight;
        public ushort biPlanes, biBitCount;
        public uint biCompression, biSizeImage;
        public int biXPelsPerMeter, biYPelsPerMeter;
        public uint biClrUsed, biClrImportant;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct BITMAPINFO
    {
        public BITMAPINFOHEADER bmiHeader;
        public uint c1, c2, c3;
    }

    [DllImport("gdi32.dll")]
    static extern IntPtr CreateDIBSection(IntPtr dc, ref BITMAPINFO bmi, uint usage, out IntPtr bits, IntPtr sect, uint off);

    /// <summary>A captured frame: 32-bit BGRA, top-down.</summary>
    public sealed class Frame
    {
        public int Width, Height;
        public byte[] Bgra;
    }

    /// <summary>Capture a screen rectangle. Negative/zero size means the whole virtual desktop.</summary>
    public static Frame CaptureScreen(int x, int y, int w, int h)
    {
        if (w <= 0 || h <= 0)
        {
            x = Native.GetSystemMetrics(Native.SM_XVIRTUALSCREEN);
            y = Native.GetSystemMetrics(Native.SM_YVIRTUALSCREEN);
            w = Native.GetSystemMetrics(Native.SM_CXVIRTUALSCREEN);
            h = Native.GetSystemMetrics(Native.SM_CYVIRTUALSCREEN);
            if (w <= 0 || h <= 0)
            {
                w = Native.GetSystemMetrics(Native.SM_CXSCREEN);
                h = Native.GetSystemMetrics(Native.SM_CYSCREEN);
            }
        }
        if (w <= 0 || h <= 0) throw new InvalidOperationException("screen has no usable size");

        IntPtr screen = Native.GetDC(IntPtr.Zero);
        try { return Blit(screen, x, y, w, h); }
        finally { Native.ReleaseDC(IntPtr.Zero, screen); }
    }

    /// <summary>Capture one window by its on-screen rectangle.</summary>
    public static Frame CaptureWindow(IntPtr hwnd)
    {
        if (!Native.GetWindowRect(hwnd, out RECT r)) throw new InvalidOperationException("GetWindowRect failed");
        return CaptureScreen(r.Left, r.Top, r.Right - r.Left, r.Bottom - r.Top);
    }

    static Frame Blit(IntPtr src, int x, int y, int w, int h)
    {
        IntPtr mem = Native.CreateCompatibleDC(src);
        var bmi = new BITMAPINFO();
        bmi.bmiHeader.biSize = (uint)Marshal.SizeOf<BITMAPINFOHEADER>();
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h;   // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0; // BI_RGB

        IntPtr dib = CreateDIBSection(src, ref bmi, 0, out IntPtr bits, IntPtr.Zero, 0);
        if (dib == IntPtr.Zero) { Native.DeleteDC(mem); throw new InvalidOperationException("CreateDIBSection failed"); }
        IntPtr old = Native.SelectObject(mem, dib);
        try
        {
            if (!Native.BitBlt(mem, 0, 0, w, h, src, x, y, Native.SRCCOPY))
                throw new InvalidOperationException("BitBlt failed, error " + Native.GetLastError());
            var buf = new byte[w * h * 4];
            Marshal.Copy(bits, buf, 0, buf.Length);
            return new Frame { Width = w, Height = h, Bgra = buf };
        }
        finally
        {
            Native.SelectObject(mem, old);
            Native.DeleteObject(dib);
            Native.DeleteDC(mem);
        }
    }

    /// <summary>Fraction of pixels that are not pure black, and the number of distinct colours.</summary>
    public static (double nonBlack, int distinct) Stats(Frame f)
    {
        long nb = 0;
        var seen = new System.Collections.Generic.HashSet<int>();
        for (int i = 0; i < f.Bgra.Length; i += 4)
        {
            int c = f.Bgra[i] | (f.Bgra[i + 1] << 8) | (f.Bgra[i + 2] << 16);
            if (c != 0) nb++;
            if (seen.Count < 4096) seen.Add(c);
        }
        long total = f.Bgra.Length / 4;
        return (total == 0 ? 0 : (double)nb / total, seen.Count);
    }

    public static void WritePng(Frame f, string path)
    {
        using var fs = File.Create(path);
        WritePng(f, fs);
    }

    public static void WritePng(Frame f, Stream fs)
    {
        Span<byte> sig = stackalloc byte[] { 137, 80, 78, 71, 13, 10, 26, 10 };
        fs.Write(sig);

        var ihdr = new byte[13];
        WriteBE(ihdr, 0, f.Width);
        WriteBE(ihdr, 4, f.Height);
        ihdr[8] = 8;   // bit depth
        ihdr[9] = 2;   // colour type: truecolour
        Chunk(fs, "IHDR", ihdr);

        // Filter type 0 per scanline, RGB from BGRA.
        var raw = new byte[(f.Width * 3 + 1) * f.Height];
        int o = 0;
        for (int yy = 0; yy < f.Height; yy++)
        {
            raw[o++] = 0;
            int rowStart = yy * f.Width * 4;
            for (int xx = 0; xx < f.Width; xx++)
            {
                int p = rowStart + xx * 4;
                raw[o++] = f.Bgra[p + 2];
                raw[o++] = f.Bgra[p + 1];
                raw[o++] = f.Bgra[p + 0];
            }
        }
        Chunk(fs, "IDAT", Zlib(raw));
        Chunk(fs, "IEND", Array.Empty<byte>());
    }

    static byte[] Zlib(byte[] data)
    {
        using var ms = new MemoryStream();
        ms.WriteByte(0x78); ms.WriteByte(0x9C);
        using (var df = new DeflateStream(ms, CompressionLevel.Fastest, true)) df.Write(data, 0, data.Length);
        uint a = 1, b = 0;
        foreach (byte x in data) { a = (a + x) % 65521; b = (b + a) % 65521; }
        uint adler = (b << 16) | a;
        ms.WriteByte((byte)(adler >> 24)); ms.WriteByte((byte)(adler >> 16));
        ms.WriteByte((byte)(adler >> 8)); ms.WriteByte((byte)adler);
        return ms.ToArray();
    }

    static void Chunk(Stream s, string type, byte[] data)
    {
        var len = new byte[4];
        WriteBE(len, 0, data.Length);
        s.Write(len, 0, 4);

        // The CRC covers the type and the data, but not the length.
        var body = new byte[4 + data.Length];
        Buffer.BlockCopy(System.Text.Encoding.ASCII.GetBytes(type), 0, body, 0, 4);
        Buffer.BlockCopy(data, 0, body, 4, data.Length);
        s.Write(body, 0, body.Length);

        var crc = new byte[4];
        WriteBE(crc, 0, unchecked((int)Crc32(body)));
        s.Write(crc, 0, 4);
    }

    static readonly uint[] Table = BuildTable();
    static uint[] BuildTable()
    {
        var t = new uint[256];
        for (uint n = 0; n < 256; n++)
        {
            uint c = n;
            for (int k = 0; k < 8; k++) c = (c & 1) != 0 ? 0xEDB88320 ^ (c >> 1) : c >> 1;
            t[n] = c;
        }
        return t;
    }

    static uint Crc32(byte[] d)
    {
        uint c = 0xFFFFFFFF;
        foreach (byte b in d) c = Table[(c ^ b) & 0xFF] ^ (c >> 8);
        return c ^ 0xFFFFFFFF;
    }

    static void WriteBE(byte[] b, int o, int v)
    {
        b[o] = (byte)(v >> 24); b[o + 1] = (byte)(v >> 16); b[o + 2] = (byte)(v >> 8); b[o + 3] = (byte)v;
    }
}
