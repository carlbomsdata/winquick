using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Threading;
using System.Windows.Automation;

namespace WinQuick.Ui;

/// <summary>
/// The guest half of `winquick desktop`.
///
/// Each invocation performs one verb and prints one JSON object on stdout, so
/// the host side never has to parse human prose and a failure is always
/// distinguishable from an empty result. Everything here runs inside the
/// Windows guest; the host talks to it through the ordinary WinQuick mailbox.
/// </summary>
public static class Program
{
    public static int Main(string[] argv)
    {
        try
        {
            if (argv.Length > 0 && argv[0] == "serve") return Serve(new Args(argv));
            var args = new Args(argv);
            JsonObject result = Dispatch(args);
            result["ok"] = true;
            Print(result);
            return 0;
        }
        catch (Exception ex)
        {
            var o = new JsonObject { ["ok"] = false, ["error"] = ex.Message, ["kind"] = ex.GetType().Name };
            Print(o);
            return 1;
        }
    }

    /// <summary>
    /// Run verbs from the session control disk until the guest is shut down.
    ///
    /// A session cannot use the mailbox volume for this: the host would be
    /// writing to a filesystem Windows has mounted, and the two views of the
    /// allocation tables corrupt each other. See <see cref="Control"/>.
    /// </summary>
    static int Serve(Args a)
    {
        int pollMs = a.Int("poll", 15);
        using var control = Control.Open();
        Console.Out.WriteLine("wqui: serving on the control disk");
        Console.Out.Flush();

        ulong last = 0;
        while (true)
        {
            var request = control.WaitForRequest(last, pollMs);
            last = request.Seq;

            JsonObject result;
            int code;
            try
            {
                result = Dispatch(new Args(request.Argv));
                result["ok"] = true;
                code = 0;
            }
            catch (Exception ex)
            {
                result = new JsonObject { ["ok"] = false, ["error"] = ex.Message, ["kind"] = ex.GetType().Name };
                code = 1;
            }
            control.Respond(request.Seq, code, result.ToJsonString(new JsonSerializerOptions { WriteIndented = true }));
        }
    }

    static void Print(JsonObject o) =>
        Console.Out.WriteLine(o.ToJsonString(new JsonSerializerOptions { WriteIndented = true }));

    static JsonObject Dispatch(Args a) => a.Verb switch
    {
        "windows" => Windows(a),
        "display" => Display(a),
        "launch" => Launch(a),
        "wait-window" => WaitWindow(a),
        "focus" => Focus(a),
        "screenshot" => Screenshot(a),
        "tree" => Tree(a),
        "find" => Find(a),
        "get" => Get(a),
        "click" => Click(a),
        "type" => Type(a),
        "key" => Key(a),
        "select" => Select(a),
        "toggle" => Toggle(a),
        "mouse" => Mouse(a),
        _ => throw new ArgumentException($"unknown verb '{a.Verb}'"),
    };

    // ---- window discovery -------------------------------------------------

    sealed class Win
    {
        public IntPtr Hwnd; public string Title, Class; public uint Pid;
        public bool Visible; public RECT Rect;
    }

    static List<Win> AllWindows(bool visibleOnly)
    {
        var list = new List<Win>();
        Native.EnumWindows((h, _) =>
        {
            bool vis = Native.IsWindowVisible(h);
            if (visibleOnly && !vis) return true;
            Native.GetWindowRect(h, out RECT r);
            Native.GetWindowThreadProcessId(h, out uint pid);
            list.Add(new Win
            {
                Hwnd = h, Title = Native.WindowText(h), Class = Native.ClassName(h),
                Pid = pid, Visible = vis, Rect = r,
            });
            return true;
        }, IntPtr.Zero);
        return list;
    }

    static JsonObject WinJson(Win w) => new()
    {
        ["hwnd"] = w.Hwnd.ToInt64(),
        ["title"] = w.Title,
        ["class"] = w.Class,
        ["pid"] = w.Pid,
        ["visible"] = w.Visible,
        ["bounds"] = new JsonObject
        {
            ["x"] = w.Rect.Left, ["y"] = w.Rect.Top,
            ["width"] = w.Rect.Right - w.Rect.Left, ["height"] = w.Rect.Bottom - w.Rect.Top,
        },
    };

    static JsonObject Windows(Args a)
    {
        bool all = a.Flag("all");
        var arr = new JsonArray();
        foreach (var w in AllWindows(!all))
        {
            if (!all && string.IsNullOrWhiteSpace(w.Title)) continue;
            arr.Add(WinJson(w));
        }
        return new JsonObject { ["windows"] = arr, ["count"] = arr.Count };
    }

    /// <summary>Find one window by title substring, or by explicit handle.</summary>
    static Win ResolveWindow(Args a, bool required = true)
    {
        if (a.Has("hwnd"))
        {
            var h = new IntPtr(long.Parse(a.Get("hwnd")));
            Native.GetWindowRect(h, out RECT r);
            Native.GetWindowThreadProcessId(h, out uint pid);
            return new Win { Hwnd = h, Title = Native.WindowText(h), Class = Native.ClassName(h), Pid = pid, Visible = Native.IsWindowVisible(h), Rect = r };
        }
        if (a.Has("title"))
        {
            string t = a.Get("title");
            var hits = AllWindows(true).Where(w => w.Title.Contains(t, StringComparison.OrdinalIgnoreCase)).ToList();
            if (hits.Count == 0)
            {
                if (!required) return null;
                throw new InvalidOperationException($"no visible window with title containing '{t}'");
            }
            if (hits.Count > 1)
            {
                var exact = hits.Where(w => string.Equals(w.Title, t, StringComparison.OrdinalIgnoreCase)).ToList();
                if (exact.Count == 1) return exact[0];
                throw new InvalidOperationException(
                    $"{hits.Count} windows match title '{t}': " + string.Join(", ", hits.Select(w => $"\"{w.Title}\"")));
            }
            return hits[0];
        }
        if (required) throw new ArgumentException("need --title or --hwnd");
        return null;
    }

    static JsonObject WaitWindow(Args a)
    {
        int timeout = a.Int("timeout", 30000);
        var sw = Stopwatch.StartNew();
        while (sw.ElapsedMilliseconds < timeout)
        {
            var w = ResolveWindow(a, required: false);
            if (w != null && w.Rect.Right > w.Rect.Left)
                return new JsonObject { ["window"] = WinJson(w), ["waitedMs"] = sw.ElapsedMilliseconds };
            Thread.Sleep(200);
        }
        throw new TimeoutException($"no window appeared within {timeout} ms");
    }

    static JsonObject Focus(Args a)
    {
        var w = ResolveWindow(a);
        if (Native.IsIconic(w.Hwnd)) Native.ShowWindow(w.Hwnd, Native.SW_RESTORE);
        Native.BringWindowToTop(w.Hwnd);
        // Foreground changes are refused unless the calling thread shares input
        // state with the owner, so attach for the duration of the call.
        uint target = Native.GetWindowThreadProcessId(w.Hwnd, out _);
        uint self = Native.GetCurrentThreadId();
        Native.AttachThreadInput(self, target, true);
        bool ok = Native.SetForegroundWindow(w.Hwnd);
        Native.AttachThreadInput(self, target, false);
        Thread.Sleep(120);
        return new JsonObject { ["window"] = WinJson(w), ["foreground"] = ok, ["actual"] = Native.GetForegroundWindow().ToInt64() };
    }

    // ---- display diagnostics ---------------------------------------------

    static JsonObject Display(Args a)
    {
        var o = new JsonObject();
        o["screenWidth"] = Native.GetSystemMetrics(Native.SM_CXSCREEN);
        o["screenHeight"] = Native.GetSystemMetrics(Native.SM_CYSCREEN);
        o["virtualWidth"] = Native.GetSystemMetrics(Native.SM_CXVIRTUALSCREEN);
        o["virtualHeight"] = Native.GetSystemMetrics(Native.SM_CYVIRTUALSCREEN);
        o["monitors"] = Native.GetSystemMetrics(Native.SM_CMONITORS);
        o["remoteSession"] = Native.GetSystemMetrics(Native.SM_REMOTESESSION) != 0;

        Native.ProcessIdToSessionId(Native.GetCurrentProcessId(), out uint sid);
        o["sessionId"] = sid;
        o["consoleSessionId"] = Native.WTSGetActiveConsoleSessionId();

        IntPtr dc = Native.GetDC(IntPtr.Zero);
        o["dcBitsPerPixel"] = Native.GetDeviceCaps(dc, 12);   // BITSPIXEL
        o["dcWidth"] = Native.GetDeviceCaps(dc, 8);           // HORZRES
        o["dcHeight"] = Native.GetDeviceCaps(dc, 10);         // VERTRES
        Native.ReleaseDC(IntPtr.Zero, dc);

        var adapters = new JsonArray();
        for (uint i = 0; ; i++)
        {
            var dd = new DISPLAY_DEVICE();
            dd.cb = System.Runtime.InteropServices.Marshal.SizeOf<DISPLAY_DEVICE>();
            if (!Native.EnumDisplayDevicesW(null, i, ref dd, 0)) break;
            var ao = new JsonObject
            {
                ["name"] = dd.DeviceName, ["string"] = dd.DeviceString,
                ["id"] = dd.DeviceID, ["key"] = dd.DeviceKey,
                ["stateFlags"] = dd.StateFlags,
                ["attachedToDesktop"] = (dd.StateFlags & 0x1) != 0,
                ["primary"] = (dd.StateFlags & 0x4) != 0,
            };
            var dm = new DEVMODE();
            dm.dmSize = (ushort)System.Runtime.InteropServices.Marshal.SizeOf<DEVMODE>();
            if (Native.EnumDisplaySettingsW(dd.DeviceName, -1 /* ENUM_CURRENT_SETTINGS */, ref dm))
            {
                ao["currentMode"] = $"{dm.dmPelsWidth}x{dm.dmPelsHeight}x{dm.dmBitsPerPel}@{dm.dmDisplayFrequency}";
            }
            var mons = new JsonArray();
            for (uint j = 0; ; j++)
            {
                var md = new DISPLAY_DEVICE();
                md.cb = System.Runtime.InteropServices.Marshal.SizeOf<DISPLAY_DEVICE>();
                if (!Native.EnumDisplayDevicesW(dd.DeviceName, j, ref md, 0)) break;
                mons.Add(new JsonObject { ["name"] = md.DeviceName, ["string"] = md.DeviceString, ["stateFlags"] = md.StateFlags });
            }
            ao["monitors"] = mons;
            adapters.Add(ao);
        }
        o["adapters"] = adapters;
        return o;
    }

    // ---- process ----------------------------------------------------------

    static JsonObject Launch(Args a)
    {
        var rest = a.Rest;
        if (rest.Count == 0) throw new ArgumentException("launch needs a program to run");
        var psi = new ProcessStartInfo(rest[0]) { UseShellExecute = false };
        for (int i = 1; i < rest.Count; i++) psi.ArgumentList.Add(rest[i]);
        if (a.Has("cwd")) psi.WorkingDirectory = a.Get("cwd");
        var p = Process.Start(psi);
        return new JsonObject { ["pid"] = p.Id, ["program"] = rest[0] };
    }

    // ---- capture ----------------------------------------------------------

    static JsonObject Screenshot(Args a)
    {
        string path = a.Rest.FirstOrDefault() ?? a.Get("out");
        if (string.IsNullOrEmpty(path)) throw new ArgumentException("screenshot needs an output path");

        Shot.Frame f;
        string scope;
        if (a.Has("hwnd") || a.Has("title"))
        {
            var w = ResolveWindow(a);
            f = Shot.CaptureWindow(w.Hwnd);
            scope = $"window \"{w.Title}\"";
        }
        else if (a.Has("rect"))
        {
            var p = a.Get("rect").Split(',').Select(int.Parse).ToArray();
            f = Shot.CaptureScreen(p[0], p[1], p[2], p[3]);
            scope = "rect";
        }
        else { f = Shot.CaptureScreen(0, 0, 0, 0); scope = "screen"; }

        var (nonBlack, distinct) = Shot.Stats(f);
        var result = new JsonObject
        {
            ["scope"] = scope,
            ["width"] = f.Width, ["height"] = f.Height,
            ["nonBlackFraction"] = Math.Round(nonBlack, 5),
            ["distinctColors"] = distinct,
        };

        // "-" means hand the image back through the control channel rather than
        // leaving it in the guest, which is what a session wants: there is no
        // shared filesystem to drop it on.
        if (path == "-")
        {
            using var ms = new MemoryStream();
            Shot.WritePng(f, ms);
            result["pngBase64"] = Convert.ToBase64String(ms.ToArray());
        }
        else
        {
            Shot.WritePng(f, path);
            result["path"] = path;
        }
        return result;
    }

    // ---- UI automation ----------------------------------------------------

    static Uia.Selector SelectorFrom(Args a)
    {
        var sel = new Uia.Selector
        {
            AutomationId = a.Has("automation-id") ? a.Get("automation-id") : null,
            Name = a.Has("name") ? a.Get("name") : null,
            ClassName = a.Has("class") ? a.Get("class") : null,
            ControlType = a.Has("control-type") ? a.Get("control-type") : null,
        };
        var w = ResolveWindow(a, required: false);
        if (w != null) sel.Window = w.Hwnd;
        return sel;
    }

    static JsonObject Tree(Args a)
    {
        var w = ResolveWindow(a, required: false);
        var root = Uia.Root(w?.Hwnd ?? IntPtr.Zero);
        int depth = a.Int("depth", 12);
        return new JsonObject
        {
            ["root"] = w == null ? "desktop" : w.Title,
            ["tree"] = Uia.Snapshot(root, 0, depth),
        };
    }

    static JsonObject Find(Args a)
    {
        var sel = SelectorFrom(a);
        var matches = Uia.FindAll(Uia.Root(sel.Window), sel);
        var arr = new JsonArray();
        foreach (var m in matches) arr.Add(Uia.Snapshot(m, 0, 0));
        return new JsonObject { ["matches"] = arr, ["count"] = arr.Count };
    }

    static JsonObject Get(Args a) =>
        new() { ["element"] = Uia.Snapshot(Uia.Resolve(SelectorFrom(a)), 0, a.Int("depth", 0)) };

    static JsonObject Click(Args a)
    {
        var e = Uia.Resolve(SelectorFrom(a));
        string how = a.Flag("mouse") ? null : Uia.Invoke(e);
        if (how == null)
        {
            var r = e.Current.BoundingRectangle;
            if (r.Width <= 0 || double.IsInfinity(r.X))
                throw new InvalidOperationException("element supports no clickable pattern and has no on-screen bounds");
            Input.ClickAt((int)(r.X + r.Width / 2), (int)(r.Y + r.Height / 2), a.Flag("right"));
            how = "mouse";
        }
        Thread.Sleep(a.Int("settle", 250));
        return new JsonObject { ["clicked"] = Uia.Snapshot(e, 0, 0), ["via"] = how };
    }

    static JsonObject Type(Args a)
    {
        string text = a.Has("text") ? a.Get("text") : string.Join(" ", a.Rest);
        var sel = SelectorFrom(a);
        if (sel.IsEmpty)
        {
            Input.TypeText(text);
            return new JsonObject { ["typed"] = text, ["target"] = "focused" };
        }
        var e = Uia.Resolve(sel);
        Uia.SetValue(e, text);
        Thread.Sleep(a.Int("settle", 200));
        return new JsonObject { ["typed"] = text, ["element"] = Uia.Snapshot(e, 0, 0) };
    }

    static JsonObject Key(Args a)
    {
        var chords = a.Rest.Count > 0 ? a.Rest : new List<string> { a.Get("key") };
        foreach (var c in chords) { Input.PressChord(c); Thread.Sleep(40); }
        Thread.Sleep(a.Int("settle", 200));
        return new JsonObject { ["pressed"] = new JsonArray(chords.Select(c => (JsonNode)c!).ToArray()) };
    }

    static JsonObject Select(Args a)
    {
        var e = Uia.Resolve(SelectorFrom(a));
        string item = a.Get("item");

        // The item may be a child of a collapsed combo box, which only creates
        // its list when expanded.
        if (Uia.TryPattern<ExpandCollapsePattern>(e, ExpandCollapsePattern.Pattern, out var ep)
            && ep.Current.ExpandCollapseState == ExpandCollapseState.Collapsed)
        {
            ep.Expand();
            Thread.Sleep(200);
        }

        var cond = new PropertyCondition(AutomationElement.NameProperty, item);
        var target = e.FindFirst(TreeScope.Descendants, cond)
            ?? throw new InvalidOperationException($"no item named '{item}' in {SelectorFrom(a)}");

        if (Uia.TryPattern<SelectionItemPattern>(target, SelectionItemPattern.Pattern, out var sp)) sp.Select();
        else Uia.Invoke(target);

        Thread.Sleep(a.Int("settle", 250));
        return new JsonObject { ["selected"] = item, ["element"] = Uia.Snapshot(e, 0, 0) };
    }

    static JsonObject Toggle(Args a)
    {
        var e = Uia.Resolve(SelectorFrom(a));
        if (!Uia.TryPattern<TogglePattern>(e, TogglePattern.Pattern, out var tp))
            throw new InvalidOperationException("element does not support Toggle");
        if (a.Has("state"))
        {
            var want = a.Get("state").Equals("on", StringComparison.OrdinalIgnoreCase) ? ToggleState.On : ToggleState.Off;
            int guard = 0;
            while (tp.Current.ToggleState != want && guard++ < 3) { tp.Toggle(); Thread.Sleep(80); }
        }
        else tp.Toggle();
        Thread.Sleep(a.Int("settle", 200));
        return new JsonObject { ["element"] = Uia.Snapshot(e, 0, 0) };
    }

    static JsonObject Mouse(Args a)
    {
        int x = a.Int("x", -1), y = a.Int("y", -1);
        if (x < 0 || y < 0) throw new ArgumentException("mouse needs --x and --y");
        if (a.Flag("move")) Input.MoveMouse(x, y);
        else Input.ClickAt(x, y, a.Flag("right"));
        Thread.Sleep(a.Int("settle", 200));
        Native.GetCursorPos(out POINT p);
        return new JsonObject { ["x"] = p.X, ["y"] = p.Y };
    }
}

/// <summary>A tiny `--flag value` parser; the guest side has no dependencies to spare.</summary>
sealed class Args
{
    public string Verb { get; }
    public List<string> Rest { get; } = new();
    readonly Dictionary<string, string> _opts = new(StringComparer.OrdinalIgnoreCase);

    public Args(string[] argv)
    {
        if (argv.Length == 0) throw new ArgumentException("usage: wqui <verb> [options]");
        Verb = argv[0];
        for (int i = 1; i < argv.Length; i++)
        {
            string s = argv[i];
            if (s.StartsWith("--"))
            {
                string k = s[2..];
                int eq = k.IndexOf('=');
                if (eq >= 0) { _opts[k[..eq]] = k[(eq + 1)..]; continue; }
                if (i + 1 < argv.Length && !argv[i + 1].StartsWith("--")) _opts[k] = argv[++i];
                else _opts[k] = "true";
            }
            else Rest.Add(s);
        }
    }

    public bool Has(string k) => _opts.ContainsKey(k);
    public string Get(string k) => _opts.TryGetValue(k, out var v) ? v : throw new ArgumentException($"missing --{k}");
    public bool Flag(string k) => _opts.TryGetValue(k, out var v) && v != "false";
    public int Int(string k, int dflt) => _opts.TryGetValue(k, out var v) && int.TryParse(v, out int n) ? n : dflt;
}
