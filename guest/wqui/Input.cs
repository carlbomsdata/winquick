using System;
using System.Collections.Generic;
using System.Threading;

namespace WinQuick.Ui;

/// <summary>Synthetic keyboard and mouse input, delivered through SendInput.</summary>
public static class Input
{
    static readonly Dictionary<string, ushort> Keys = new(StringComparer.OrdinalIgnoreCase)
    {
        ["enter"] = 0x0D, ["return"] = 0x0D, ["tab"] = 0x09, ["escape"] = 0x1B, ["esc"] = 0x1B,
        ["space"] = 0x20, ["backspace"] = 0x08, ["back"] = 0x08, ["delete"] = 0x2E, ["del"] = 0x2E,
        ["insert"] = 0x2D, ["home"] = 0x24, ["end"] = 0x23, ["pageup"] = 0x21, ["pagedown"] = 0x22,
        ["up"] = 0x26, ["down"] = 0x28, ["left"] = 0x25, ["right"] = 0x27,
        ["f1"] = 0x70, ["f2"] = 0x71, ["f3"] = 0x72, ["f4"] = 0x73, ["f5"] = 0x74, ["f6"] = 0x75,
        ["f7"] = 0x76, ["f8"] = 0x77, ["f9"] = 0x78, ["f10"] = 0x79, ["f11"] = 0x7A, ["f12"] = 0x7B,
    };

    static readonly Dictionary<string, ushort> Modifiers = new(StringComparer.OrdinalIgnoreCase)
    {
        ["ctrl"] = 0x11, ["control"] = 0x11, ["alt"] = 0x12, ["shift"] = 0x10, ["win"] = 0x5B,
    };

    /// <summary>Press a chord such as "Enter", "ctrl+s" or "alt+F4".</summary>
    public static void PressChord(string chord)
    {
        var parts = chord.Split('+', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        if (parts.Length == 0) throw new ArgumentException("empty key");

        var mods = new List<ushort>();
        for (int i = 0; i < parts.Length - 1; i++)
        {
            if (!Modifiers.TryGetValue(parts[i], out ushort m))
                throw new ArgumentException($"unknown modifier '{parts[i]}'");
            mods.Add(m);
        }

        string last = parts[^1];
        ushort vk;
        if (Keys.TryGetValue(last, out vk)) { }
        else if (last.Length == 1)
        {
            short scan = Native.VkKeyScanW(last[0]);
            if (scan == -1) throw new ArgumentException($"cannot type '{last}' with the current layout");
            vk = (ushort)(scan & 0xFF);
            if ((scan & 0x100) != 0) mods.Add(0x10); // shift
        }
        else throw new ArgumentException($"unknown key '{last}'");

        foreach (var m in mods) Send(KeyInput(m, false));
        Send(KeyInput(vk, false));
        Send(KeyInput(vk, true));
        for (int i = mods.Count - 1; i >= 0; i--) Send(KeyInput(mods[i], true));
    }

    /// <summary>Type literal text as Unicode, so it does not depend on the keyboard layout.</summary>
    public static void TypeText(string text)
    {
        foreach (char ch in text)
        {
            if (ch == '\n' || ch == '\r') { PressChord("enter"); continue; }
            if (ch == '\t') { PressChord("tab"); continue; }
            Send(UnicodeInput(ch, false));
            Send(UnicodeInput(ch, true));
            Thread.Sleep(2);
        }
    }

    public static void MoveMouse(int x, int y)
    {
        int vx = Native.GetSystemMetrics(Native.SM_XVIRTUALSCREEN);
        int vy = Native.GetSystemMetrics(Native.SM_YVIRTUALSCREEN);
        int vw = Math.Max(1, Native.GetSystemMetrics(Native.SM_CXVIRTUALSCREEN));
        int vh = Math.Max(1, Native.GetSystemMetrics(Native.SM_CYVIRTUALSCREEN));

        var i = new INPUT { type = Native.INPUT_MOUSE };
        i.U.mi.dx = (int)(((double)(x - vx) * 65535) / vw);
        i.U.mi.dy = (int)(((double)(y - vy) * 65535) / vh);
        i.U.mi.dwFlags = Native.MOUSEEVENTF_MOVE | Native.MOUSEEVENTF_ABSOLUTE | Native.MOUSEEVENTF_VIRTUALDESK;
        Send(i);
        Native.SetCursorPos(x, y);
    }

    public static void ClickAt(int x, int y, bool right = false)
    {
        MoveMouse(x, y);
        Thread.Sleep(30);
        var down = new INPUT { type = Native.INPUT_MOUSE };
        down.U.mi.dwFlags = right ? Native.MOUSEEVENTF_RIGHTDOWN : Native.MOUSEEVENTF_LEFTDOWN;
        var up = new INPUT { type = Native.INPUT_MOUSE };
        up.U.mi.dwFlags = right ? Native.MOUSEEVENTF_RIGHTUP : Native.MOUSEEVENTF_LEFTUP;
        Send(down);
        Thread.Sleep(30);
        Send(up);
    }

    static INPUT KeyInput(ushort vk, bool up)
    {
        var i = new INPUT { type = Native.INPUT_KEYBOARD };
        i.U.ki.wVk = vk;
        i.U.ki.wScan = (ushort)Native.MapVirtualKeyW(vk, 0);
        i.U.ki.dwFlags = up ? Native.KEYEVENTF_KEYUP : 0;
        return i;
    }

    static INPUT UnicodeInput(char ch, bool up)
    {
        var i = new INPUT { type = Native.INPUT_KEYBOARD };
        i.U.ki.wScan = ch;
        i.U.ki.dwFlags = Native.KEYEVENTF_UNICODE | (up ? Native.KEYEVENTF_KEYUP : 0);
        return i;
    }

    static void Send(INPUT i)
    {
        uint n = Native.SendInput(1, new[] { i }, System.Runtime.InteropServices.Marshal.SizeOf<INPUT>());
        if (n == 0) throw new InvalidOperationException("SendInput was blocked, error " + Native.GetLastError());
    }
}
