using System;
using System.Windows.Forms;

namespace XpPanel
{
    // Deliberately confined to APIs present in .NET Framework 4.0 and in the
    // Win32 surface available on Windows XP SP3: no async, no newer BCL types.
    static class Program
    {
        [STAThread]
        static void Main()
        {
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            Application.Run(new MainForm());
        }
    }
}
