using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json.Nodes;
using System.Windows.Automation;

namespace WinQuick.Ui;

/// <summary>
/// The semantic view of the guest UI.
///
/// Everything an automation client does is expressed against UI Automation
/// rather than pixels: elements are addressed by AutomationId or Name, and
/// actions go through the control patterns the element actually supports. A
/// selector that matches more than one element is an error, never a guess —
/// clicking an arbitrary one of two buttons is the kind of failure that looks
/// like success in a log.
/// </summary>
public static class Uia
{
    public sealed class Selector
    {
        public string AutomationId, Name, ClassName, ControlType;
        public IntPtr Window = IntPtr.Zero;

        public bool IsEmpty => AutomationId == null && Name == null && ClassName == null && ControlType == null;

        public override string ToString()
        {
            var parts = new List<string>();
            if (AutomationId != null) parts.Add($"automation-id={AutomationId}");
            if (Name != null) parts.Add($"name={Name}");
            if (ClassName != null) parts.Add($"class={ClassName}");
            if (ControlType != null) parts.Add($"control-type={ControlType}");
            return string.Join(" ", parts);
        }
    }

    public static AutomationElement Root(IntPtr hwnd) =>
        hwnd == IntPtr.Zero ? AutomationElement.RootElement : AutomationElement.FromHandle(hwnd);

    /// <summary>Resolve a selector to exactly one element, or explain why not.</summary>
    public static AutomationElement Resolve(Selector sel)
    {
        if (sel.IsEmpty) throw new ArgumentException("no selector given; use --automation-id, --name, --class or --control-type");
        var matches = FindAll(Root(sel.Window), sel);
        if (matches.Count == 0)
            throw new InvalidOperationException($"no element matches {sel}");
        if (matches.Count > 1)
        {
            var described = matches.Take(5).Select(Describe);
            throw new InvalidOperationException(
                $"{matches.Count} elements match {sel}; narrow the selector. Candidates: " + string.Join("; ", described));
        }
        return matches[0];
    }

    static string Describe(AutomationElement e)
    {
        try
        {
            var c = e.Current;
            return $"[id={c.AutomationId} name=\"{c.Name}\" type={c.ControlType.ProgrammaticName}]";
        }
        catch { return "[unavailable]"; }
    }

    public static List<AutomationElement> FindAll(AutomationElement root, Selector sel)
    {
        var conds = new List<Condition>();
        if (sel.AutomationId != null) conds.Add(new PropertyCondition(AutomationElement.AutomationIdProperty, sel.AutomationId));
        if (sel.Name != null) conds.Add(new PropertyCondition(AutomationElement.NameProperty, sel.Name));
        if (sel.ClassName != null) conds.Add(new PropertyCondition(AutomationElement.ClassNameProperty, sel.ClassName));
        if (sel.ControlType != null) conds.Add(new PropertyCondition(AutomationElement.ControlTypeProperty, ParseControlType(sel.ControlType)));

        Condition cond = conds.Count == 1 ? conds[0] : new AndCondition(conds.ToArray());
        var found = root.FindAll(TreeScope.Descendants, cond);
        var list = new List<AutomationElement>();
        foreach (AutomationElement e in found) list.Add(e);
        return list;
    }

    public static ControlType ParseControlType(string name)
    {
        var field = typeof(ControlType).GetField(name, System.Reflection.BindingFlags.Public | System.Reflection.BindingFlags.Static
                                                        | System.Reflection.BindingFlags.IgnoreCase);
        if (field == null) throw new ArgumentException($"unknown control type '{name}'");
        return (ControlType)field.GetValue(null);
    }

    public static JsonObject Snapshot(AutomationElement e, int depth, int maxDepth)
    {
        var o = new JsonObject();
        AutomationElement.AutomationElementInformation c;
        try { c = e.Current; }
        catch (Exception ex) { o["error"] = ex.Message; return o; }

        o["automationId"] = Empty(c.AutomationId);
        o["name"] = Empty(c.Name);
        o["className"] = Empty(c.ClassName);
        o["controlType"] = c.ControlType?.ProgrammaticName?.Replace("ControlType.", "");
        o["enabled"] = c.IsEnabled;
        o["offscreen"] = c.IsOffscreen;
        if (c.NativeWindowHandle != 0) o["hwnd"] = c.NativeWindowHandle;

        var r = c.BoundingRectangle;
        if (!double.IsInfinity(r.X) && r.Width > 0)
        {
            o["bounds"] = new JsonObject
            {
                ["x"] = (int)r.X, ["y"] = (int)r.Y, ["width"] = (int)r.Width, ["height"] = (int)r.Height,
            };
        }

        var patterns = new JsonArray();
        foreach (var p in e.GetSupportedPatterns()) patterns.Add(p.ProgrammaticName.Replace("PatternIdentifiers.Pattern", ""));
        if (patterns.Count > 0) o["patterns"] = patterns;

        string value = TryValue(e);
        if (value != null) o["value"] = value;
        if (TryPattern<TogglePattern>(e, TogglePattern.Pattern, out var tp)) o["toggleState"] = tp.Current.ToggleState.ToString();
        if (TryPattern<SelectionItemPattern>(e, SelectionItemPattern.Pattern, out var sp)) o["selected"] = sp.Current.IsSelected;
        if (TryPattern<ExpandCollapsePattern>(e, ExpandCollapsePattern.Pattern, out var ep))
            o["expandState"] = ep.Current.ExpandCollapseState.ToString();

        if (depth < maxDepth)
        {
            var kids = new JsonArray();
            try
            {
                var walker = TreeWalker.ControlViewWalker;
                for (var k = walker.GetFirstChild(e); k != null; k = walker.GetNextSibling(k))
                    kids.Add(Snapshot(k, depth + 1, maxDepth));
            }
            catch { /* a control can disappear mid-walk; report what we have */ }
            if (kids.Count > 0) o["children"] = kids;
        }
        return o;
    }

    static string Empty(string s) => string.IsNullOrEmpty(s) ? null : s;

    public static string TryValue(AutomationElement e)
    {
        if (TryPattern<ValuePattern>(e, ValuePattern.Pattern, out var vp)) return vp.Current.Value;
        // A non-editable combo box or list exposes no value at all, only a
        // selection. Reporting nothing for "which item is chosen" is unhelpful
        // for the control people most want to read.
        if (TryPattern<SelectionPattern>(e, SelectionPattern.Pattern, out var sel))
        {
            try
            {
                var chosen = sel.Current.GetSelection();
                if (chosen.Length > 0) return string.Join(", ", chosen.Select(c => c.Current.Name));
            }
            catch { }
        }
        if (TryPattern<TextPattern>(e, TextPattern.Pattern, out var tp))
        {
            try { return tp.DocumentRange.GetText(4096); } catch { }
        }
        if (TryPattern<RangeValuePattern>(e, RangeValuePattern.Pattern, out var rp))
            return rp.Current.Value.ToString(System.Globalization.CultureInfo.InvariantCulture);
        return null;
    }

    public static bool TryPattern<T>(AutomationElement e, AutomationPattern id, out T pattern) where T : class
    {
        pattern = null;
        try
        {
            if (e.TryGetCurrentPattern(id, out object o)) { pattern = o as T; return pattern != null; }
        }
        catch { }
        return false;
    }

    /// <summary>Invoke an element through the best pattern it supports.</summary>
    public static string Invoke(AutomationElement e)
    {
        if (TryPattern<InvokePattern>(e, InvokePattern.Pattern, out var ip)) { ip.Invoke(); return "Invoke"; }
        if (TryPattern<TogglePattern>(e, TogglePattern.Pattern, out var tp)) { tp.Toggle(); return "Toggle"; }
        if (TryPattern<SelectionItemPattern>(e, SelectionItemPattern.Pattern, out var sp)) { sp.Select(); return "SelectionItem"; }
        if (TryPattern<ExpandCollapsePattern>(e, ExpandCollapsePattern.Pattern, out var ep))
        {
            var s = ep.Current.ExpandCollapseState;
            if (s == ExpandCollapseState.Collapsed) ep.Expand(); else ep.Collapse();
            return "ExpandCollapse";
        }
        return null;   // caller falls back to a synthetic mouse click
    }

    public static void SetValue(AutomationElement e, string text)
    {
        if (TryPattern<ValuePattern>(e, ValuePattern.Pattern, out var vp) && !vp.Current.IsReadOnly)
        {
            vp.SetValue(text);
            return;
        }
        // No ValuePattern (or read-only): focus it and type, which is what a
        // person would do and works for controls that only accept real input.
        e.SetFocus();
        System.Threading.Thread.Sleep(80);
        Input.TypeText(text);
    }
}
