using System;
using System.Drawing;
using System.Linq;

namespace ClassicNetFxX64
{
    /// <summary>
    /// Prints what the guest can only answer by actually running a .NET
    /// Framework executable. Every line is something that was once a failure.
    /// </summary>
    internal static class Program
    {
        private static int Main()
        {
            // 8 on x64, 4 on x86. The project asks for x64 and the guest is
            // ARM64, so this is the emulator answering.
            Console.WriteLine("ptr=" + IntPtr.Size);

            // A Framework without GDI+ throws TypeInitializationException here.
            using (var bmp = new Bitmap(8, 8))
            using (var g = Graphics.FromImage(bmp))
            {
                g.Clear(Color.Red);
                Console.WriteLine("bitmap=" + bmp.Width + "x" + bmp.Height +
                                  " red=" + bmp.GetPixel(1, 1).R);
            }

            // Markup compilation leaves the page behind as BAML. If
            // PresentationBuildTasks did not run, there is no resource.
            var names = typeof(Program).Assembly.GetManifestResourceNames();
            Console.WriteLine("resources=" + string.Join(",", names));
            Console.WriteLine("baml=" +
                (names.Any(n => n.EndsWith(".g.resources", StringComparison.OrdinalIgnoreCase))
                    ? "yes" : "no"));

            Console.WriteLine("clr=" + Environment.Version);
            using (var k = Microsoft.Win32.Registry.LocalMachine.OpenSubKey(
                       @"SOFTWARE\Microsoft\NET Framework Setup\NDP\v4\Full"))
            {
                Console.WriteLine("ndp-version=" +
                    (k == null ? "none" : Convert.ToString(k.GetValue("Version"))));
                Console.WriteLine("ndp-release=" +
                    (k == null ? "none" : Convert.ToString(k.GetValue("Release"))));
            }
            return 0;
        }
    }
}
