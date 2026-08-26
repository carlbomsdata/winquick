#!/usr/bin/env python3
"""Desktop and UI Automation tests driven entirely through MCP.

Every action here goes over the MCP transport — no `winquick desktop` commands —
because the point is to prove the MCP path, not the CLI path underneath it.

    python3 tests/mcp_desktop.py <winquick> <published-wpf-dir> [published-winforms-dir]
"""

import base64
import json
import struct
import subprocess
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mcp import Client, check, ok, bad  # noqa: E402
import mcp as harness  # noqa: E402

WPF = sys.argv[2] if len(sys.argv) > 2 else "/tmp/wqdemo/publish"
FORMS = sys.argv[3] if len(sys.argv) > 3 else None


def png_size(data):
    """Width and height straight out of the IHDR chunk."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        return None
    return struct.unpack(">II", data[16:24])


def test_wpf(c):
    print("== WPF through MCP ==")
    r = c.call("desktop_start", {"app": WPF})
    check("desktop_start succeeds", r["isError"], False)
    check("session reports running", r.get("running"), True)
    if not r.get("alreadyRunning"):
        ok(f"startupDurationMs={r.get('startupDurationMs')}")

    r = c.call("desktop_status")
    check("desktop_status sees the session", r.get("running"), True)
    check("desktop_status reports a pid", isinstance(r.get("pid"), int), True)

    r = c.call("desktop_launch", {"program": r"app\DemoApp.exe"})
    check("desktop_launch succeeds", r["isError"], False)

    r = c.call("desktop_wait_window", {"title": "WinQuick Demo", "timeoutMs": 60000})
    check("desktop_wait_window finds the window", r["isError"], False)
    hwnd = r.get("hwnd")
    check("a window handle came back", isinstance(hwnd, int) and hwnd > 0, True)

    r = c.call("ui_tree", {"title": "WinQuick Demo"})
    check("ui_tree succeeds", r["isError"], False)
    tree = r.get("tree") or r
    text = json.dumps(tree)
    check("the tree contains the status element", "StatusText" in text, True)
    check("the tree exposes control types", "controlType" in text or "ControlType" in text, True)

    r = c.call("ui_get", {"automationId": "StatusText", "title": "WinQuick Demo"})
    check("ui_get reads an element by automationId", r["isError"], False)
    el = r.get("element", {})
    check("ui_get returns the element name", "name" in el, True)

    r = c.call("ui_type", {"automationId": "NameBox", "text": "Tobias åäö", "title": "WinQuick Demo"})
    check("ui_type succeeds", r["isError"], False)

    r = c.call("ui_click", {"automationId": "SaveButton", "title": "WinQuick Demo"})
    check("ui_click succeeds", r["isError"], False)

    r = c.call("ui_get", {"automationId": "StatusText", "title": "WinQuick Demo"})
    name = (r.get("element") or {}).get("name", "")
    check("the click changed the status text", "Tobias" in name, True)

    # Unicode must survive the whole path: MCP -> bridge -> Windows -> back.
    check("unicode survived the round trip", "åäö" in name, True)

    r = c.call("ui_get", {"automationId": "NoSuchElement", "title": "WinQuick Demo"})
    check("a missing element is a readable tool error", r["isError"], True)

    r = c.call("ui_screenshot", {"title": "WinQuick Demo"})
    check("ui_screenshot succeeds", r["isError"], False)
    img = [x for x in r["_content"] if x.get("type") == "image"]
    check("an image content block came back", len(img), 1)
    if img:
        check("the image is declared as PNG", img[0]["mimeType"], "image/png")
        raw = base64.b64decode(img[0]["data"])
        size = png_size(raw)
        check("the payload is a real PNG", size is not None, True)
        check("the PNG is not empty", len(raw) > 1000, True)
        if size:
            ok(f"screenshot {size[0]}x{size[1]}, {len(raw)} bytes")
            check("dimensions match the reported metadata", [r.get("width"), r.get("height")], list(size))
        # A blank QEMU framebuffer would be all black; this must be real content.
        check("the capture is not a blank framebuffer", r.get("nonBlackFraction", 0) > 0.5, True)

    r = c.call("ui_screenshot", {"hwnd": hwnd})
    check("ui_screenshot accepts an hwnd", r["isError"], False)


def test_winforms(c):
    if not FORMS:
        print("== WinForms: skipped (no published directory given) ==")
        return
    print("== WinForms through MCP ==")
    c.call("desktop_stop")
    r = c.call("desktop_start", {"app": FORMS})
    check("desktop_start with the WinForms build", r["isError"], False)
    r = c.call("desktop_launch", {"program": r"app\FormsDemo.exe"})
    check("desktop_launch succeeds", r["isError"], False)
    r = c.call("desktop_wait_window", {"title": "Forms Demo", "timeoutMs": 60000})
    check("the WinForms window appears", r["isError"], False)

    # Control.Name was set, so these controls do have an AutomationId.
    r = c.call("ui_type", {"automationId": "DeviceBox", "text": "PLC-01", "title": "Forms Demo"})
    check("ui_type by automationId (Control.Name was set)", r["isError"], False)
    r = c.call("ui_click", {"automationId": "LiveCheck", "title": "Forms Demo"})
    check("ui_click a checkbox by automationId", r["isError"], False)
    r = c.call("ui_click", {"automationId": "SaveButton", "title": "Forms Demo"})
    check("ui_click a button by automationId", r["isError"], False)
    r = c.call("ui_get", {"automationId": "StatusLabel", "title": "Forms Demo"})
    name = (r.get("element") or {}).get("name", "")
    check("the WinForms app reacted", "PLC-01" in name, True)
    check("the checkbox state was read by the app", "live" in name, True)

    # The control with no Name has no AutomationId; name + controlType is the
    # documented fallback, and it must actually work.
    r = c.call("ui_get", {"automationId": "NoNameHere", "title": "Forms Demo"})
    check("a control without Control.Name has no automationId", r["isError"], True)
    r = c.call("ui_get", {"name": "No Name Here", "controlType": "Button", "title": "Forms Demo"})
    check("name + controlType reaches it instead", r["isError"], False)

    r = c.call("ui_screenshot", {"title": "Forms Demo"})
    check("WinForms screenshot succeeds", r["isError"], False)


def test_lifecycle(c):
    print("== desktop lifecycle through MCP ==")
    r = c.call("desktop_start", {"app": WPF})
    check("starting when already running is idempotent", r.get("alreadyRunning"), True)
    r = c.call("desktop_stop")
    check("desktop_stop succeeds", r["isError"], False)
    check("desktop_stop reports it was running", r.get("wasRunning"), True)
    r = c.call("desktop_status")
    check("no session after stop", r.get("running"), False)
    qemu = subprocess.run(["pgrep", "-f", "qemu-system-aarch64"], capture_output=True, text=True).stdout.split()
    check("no QEMU process left behind", len(qemu), 0)


def test_server_exit_stops_its_own_session():
    print("== a session started over MCP does not outlive the server ==")
    c = Client()
    c.initialize()
    r = c.call("desktop_start", {"app": WPF})
    if r["isError"]:
        bad("desktop_start for the lifetime test", r["_content"][0]["text"][:100])
        c.close()
        return
    ok("session started over MCP")
    c.close()  # closing stdin is how a client goes away
    qemu = subprocess.run(["pgrep", "-f", "qemu-system-aarch64"], capture_output=True, text=True).stdout.split()
    check("the desktop VM was cleaned up when the client left", len(qemu), 0)


def main():
    harness.WQ = sys.argv[1] if len(sys.argv) > 1 else "./target/release/winquick"
    print(f"MCP desktop tests against {harness.WQ}\n")
    c = Client()
    c.initialize()
    try:
        test_wpf(c)
        test_winforms(c)
        test_lifecycle(c)
    finally:
        c.call("desktop_stop")
        c.close()
    test_server_exit_stops_its_own_session()
    print(f"\n== {harness.PASS} passed, {harness.FAIL} failed ==")
    return 1 if harness.FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
