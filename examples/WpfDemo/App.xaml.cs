using System;
using System.IO;
using System.Windows;
using System.Windows.Threading;

namespace DemoApp;

public partial class App : Application
{
    const string Log = @"C:\wqcrash.txt";

    protected override void OnStartup(StartupEventArgs e)
    {
        AppDomain.CurrentDomain.UnhandledException += (_, a) => Write("AppDomain", a.ExceptionObject as Exception);
        DispatcherUnhandledException += (_, a) => { Write("Dispatcher", a.Exception); a.Handled = true; };
        Write("startup", null);
        base.OnStartup(e);
    }

    static void Write(string where, Exception ex)
    {
        try
        {
            File.AppendAllText(Log, $"=== {where} ===\n{ex}\n\n");
        }
        catch { }
    }
}
