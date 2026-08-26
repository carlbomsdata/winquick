#!/usr/bin/env python3
"""Protocol-level tests for `winquick mcp`.

These drive the real binary over stdin/stdout exactly as an MCP client does.
Nothing here imports WinQuick internals, so a passing run means the wire
behaviour is right, not merely that the Rust unit tests agree with themselves.

    python3 tests/mcp.py [path-to-winquick]

The Windows-touching checks are skipped unless a runtime is installed, so this
is safe to run on a machine that has never run `winquick setup`.
"""

import json
import os
import subprocess
import sys
import tempfile
import shutil
import hashlib
from pathlib import Path

WQ = sys.argv[1] if len(sys.argv) > 1 else "./target/release/winquick"
PASS = 0
FAIL = 0


def check(name, got, want):
    global PASS, FAIL
    if got == want:
        PASS += 1
        print(f"  PASS  {name}")
    else:
        FAIL += 1
        print(f"  FAIL  {name} -- got [{got}] want [{want}]")


def ok(name):
    global PASS
    PASS += 1
    print(f"  PASS  {name}")


def bad(name, detail):
    global FAIL
    FAIL += 1
    print(f"  FAIL  {name} -- {detail}")


class Client:
    """A minimal MCP client: one JSON object per line, both directions."""

    def __init__(self, env=None):
        self.p = subprocess.Popen(
            [WQ, "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            env=env,
        )
        self.n = 0
        self.raw_out = []

    def raw(self, line):
        self.p.stdin.write(line + "\n")
        self.p.stdin.flush()
        out = self.p.stdout.readline()
        if out:
            self.raw_out.append(out.rstrip("\n"))
        return out

    def send(self, method, params=None, notify=False):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        if not notify:
            self.n += 1
            msg["id"] = self.n
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()
        if notify:
            return None
        line = self.p.stdout.readline()
        if not line:
            raise RuntimeError("server closed stdout unexpectedly")
        self.raw_out.append(line.rstrip("\n"))
        return json.loads(line)

    def initialize(self):
        r = self.send(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "winquick-tests", "version": "1"},
            },
        )
        self.send("notifications/initialized", notify=True)
        return r

    def call(self, name, args=None):
        r = self.send("tools/call", {"name": name, "arguments": args or {}})
        if "error" in r:
            return {"_rpc_error": r["error"]}
        res = r["result"]
        out = {"isError": res.get("isError"), "_content": res.get("content", [])}
        out.update(res.get("structuredContent") or {})
        return out

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(timeout=120)
        except Exception:
            self.p.kill()
        return self.p.stderr.read()


def have_runtime():
    root = Path(os.environ.get("HOME", "")) / ".winquick"
    return (root / "images" / "validation-arm64" / "base.qcow2").exists()


# ------------------------------------------------------------------ protocol

def test_lifecycle():
    print("== MCP lifecycle ==")
    c = Client()
    r = c.initialize()
    check("initialize returns a result", "result" in r, True)
    check("protocol version echoed", r["result"]["protocolVersion"], "2024-11-05")
    check("server identifies itself", r["result"]["serverInfo"]["name"], "winquick")
    caps = r["result"]["capabilities"]
    check("advertises tools", "tools" in caps, True)
    check(
        "advertises nothing it does not implement",
        [k for k in ("prompts", "resources", "sampling", "elicitation") if k in caps],
        [],
    )
    check("instructions are present for the agent", len(r["result"].get("instructions", "")) > 100, True)

    r = c.send("ping")
    check("ping answers", r["result"], {})

    r = c.send("tools/list")
    tools = r["result"]["tools"]
    check("tools/list returns the full set", len(tools), 13)
    schema_ok = all(
        t.get("name") and t.get("description") and t.get("inputSchema", {}).get("type") == "object"
        for t in tools
    )
    check("every tool has a name, description and object schema", schema_ok, True)
    long_enough = all(len(t["description"]) > 80 for t in tools)
    check("every description explains when to use the tool", long_enough, True)
    c.close()


def test_errors():
    print("== MCP error handling ==")
    c = Client()
    c.initialize()

    r = c.send("no/such/method")
    check("unknown method -> -32601", r["error"]["code"], -32601)

    r = c.send("tools/call", {"name": "not_a_tool", "arguments": {}})
    check("unknown tool -> -32601", r["error"]["code"], -32601)

    r = c.send("tools/call", {})
    check("tools/call without a name -> -32602", r["error"]["code"], -32602)

    out = c.raw("{this is not json")
    d = json.loads(out)
    check("malformed JSON -> -32700", d["error"]["code"], -32700)
    check("malformed JSON answered with a null id", d["id"], None)

    r = c.send("ping")
    check("server still usable after malformed input", r["result"], {})

    r = c.send("tools/call", {"name": "windows_run", "arguments": {}})
    check("a tool-level failure is not an RPC error", "error" in r, False)
    check("a tool-level failure sets isError", r["result"]["isError"], True)

    # Notifications must never be answered. Send one, then a ping: if the
    # notification had produced output the ping would read it instead.
    c.send("notifications/initialized", notify=True)
    c.send("notifications/cancelled", params={"requestId": 1}, notify=True)
    r = c.send("ping")
    check("notifications produce no response", r["result"], {})

    c.close()


def test_stdout_is_protocol_only():
    print("== stdout discipline ==")
    c = Client()
    c.initialize()
    c.send("tools/list")
    c.send("ping")
    # winquick_doctor and winquick_info both run code that prints on the CLI.
    c.call("winquick_info")
    c.call("winquick_doctor")
    # Reaching the desktop with no session prints nothing but exercises the path.
    c.call("desktop_status")
    err = c.close()

    bad_lines = []
    for line in c.raw_out:
        if not line.strip():
            continue
        try:
            json.loads(line)
        except json.JSONDecodeError:
            bad_lines.append(line[:120])
    check("every stdout line is valid JSON", bad_lines, [])
    # initialize, tools/list, ping, info, doctor, desktop_status: six requests,
    # six responses, and nothing else on the channel.
    check("exactly one response per request, nothing else", len(c.raw_out), 6)
    # stderr is allowed to contain anything; it just must not be stdout.
    ok(f"diagnostics went to stderr ({len(err)} bytes)")


def test_clean_shutdown():
    print("== shutdown ==")
    c = Client()
    c.initialize()
    c.close()
    check("server exits 0 on EOF", c.p.returncode, 0)
    # Nothing MCP-specific may survive the process.
    leftover = subprocess.run(
        ["pgrep", "-f", "winquick mcp"], capture_output=True, text=True
    ).stdout.strip()
    check("no MCP process left behind", leftover, "")


# ------------------------------------------------------------------- windows

def test_windows_run():
    print("== windows_run through MCP ==")
    c = Client()
    c.initialize()

    r = c.call("windows_run", {"program": "cmd", "args": ["/c", "ver"]})
    check("cmd /c ver succeeds", r["isError"], False)
    check("exit code 0", r.get("exitCode"), 0)
    check("real Windows answered", "10.0.26100" in r.get("stdout", ""), True)
    ok(f"reported durationMs={r.get('durationMs')}")

    r = c.call("windows_run", {"program": "cmd", "args": ["/c", "exit 42"]})
    check("a non-zero exit is a result, not a failure", r["isError"], False)
    check("exit code 42 is reported", r.get("exitCode"), 42)

    r = c.call("windows_run", {"program": "cmd", "args": ["/c", "echo out & echo err 1>&2"]})
    check("stdout captured", "out" in r.get("stdout", ""), True)
    check("stderr captured separately", "err" in r.get("stderr", ""), True)

    # The v0.2.1 quoting work must survive the MCP layer unchanged.
    for desc, args, want in [
        ("embedded quotes", ["/c", 'echo say "hi"'], 'say "hi"'),
        ("unicode", ["/c", "echo åäö-日本語"], "åäö-日本語"),
        ("percent loop", ["/c", "for /L %%i in (1,1,3) do @echo %%i"], "1"),
        ("quoted path", ["/c", 'echo "C:\\Program Files\\x"'], "Program Files"),
    ]:
        r = c.call("windows_run", {"program": "cmd", "args": args})
        check(f"quoting: {desc}", want in r.get("stdout", ""), True)

    if (Path(os.environ.get("HOME", "")) / ".winquick/capabilities/powershell.img").exists():
        r = c.call(
            "windows_run",
            {"program": "pwsh", "args": ["-NoProfile", "-Command", 'Write-Output "a&b"']},
        )
        check("pwsh metacharacter after a quote", r.get("stdout", "").strip(), "a&b")
        r = c.call(
            "windows_run",
            {"program": "pwsh", "args": ["-NoProfile", "-Command", 'Write-Output "quoted string"']},
        )
        check("pwsh quoted string", r.get("stdout", "").strip(), "quoted string")

    # Argument shapes the schema forbids must be refused, readably.
    r = c.call("windows_run", {"program": "cmd", "args": "not an array"})
    check("a string args is refused", r["isError"], True)
    r = c.call("windows_run", {"program": "cmd", "workspace": "relative"})
    check("a relative workspace is refused", r["isError"], True)

    c.close()


def tree_hash(root):
    h = hashlib.sha256()
    for p in sorted(Path(root).rglob("*")):
        rel = p.relative_to(root).as_posix()
        h.update(rel.encode())
        if p.is_file():
            h.update(p.read_bytes())
    return h.hexdigest()


def test_workspace_immutability():
    print("== workspace immutability through MCP ==")
    ws = tempfile.mkdtemp(prefix="wq-mcp-ws-")
    try:
        d = Path(ws)
        (d / "src" / "nested deep").mkdir(parents=True)
        (d / "svenska åäö").mkdir()
        (d / "日本語").mkdir()
        (d / "src" / "a.txt").write_text("original\n")
        (d / "src" / "nested deep" / "b with space.txt").write_text("original\n")
        (d / "svenska åäö" / "kaka.txt").write_text("kanelbulle\n")
        (d / "日本語" / "file.txt").write_text("こんにちは\n")
        (d / "blob.bin").write_bytes(bytes(range(256)))
        (d / "empty.txt").write_bytes(b"")
        before = tree_hash(ws)

        c = Client()
        c.initialize()
        r = c.call(
            "windows_run",
            {
                "program": "cmd",
                "args": [
                    "/c",
                    r"echo MUTATED > C:\workspace\src\a.txt & "
                    r"del C:\workspace\empty.txt & "
                    r"echo new > C:\workspace\created.txt & "
                    r"rmdir /s /q C:\workspace\日本語",
                ],
                "workspace": ws,
            },
        )
        check("the mutating run completed", r["isError"], False)
        c.close()

        after = tree_hash(ws)
        check("host workspace is byte-identical afterwards", after, before)
        check("no file was created on the host", (d / "created.txt").exists(), False)
        check("no file was deleted on the host", (d / "empty.txt").exists(), True)
    finally:
        shutil.rmtree(ws, ignore_errors=True)


def test_artifacts():
    print("== artifacts through MCP ==")
    ws = tempfile.mkdtemp(prefix="wq-mcp-art-")
    dest = tempfile.mkdtemp(prefix="wq-mcp-out-")
    try:
        d = Path(ws)
        (d / "bin" / "Release" / "net10.0").mkdir(parents=True)
        (d / "logs").mkdir()
        (d / "nested" / "path").mkdir(parents=True)
        (d / "top.dll").write_text("a")
        (d / "bin" / "Release" / "net10.0" / "App.exe").write_text("a")
        (d / "bin" / "Release" / "net10.0" / "App.dll").write_text("a")
        (d / "logs" / "build.txt").write_text("a")
        (d / "nested" / "path" / "data.json").write_text("{}")
        (d / "foo1.txt").write_text("a")
        (d / "åäö 日本語.txt").write_text("a")
        (d / "blob.bin").write_bytes(bytes(range(64)))
        (d / "zero.txt").write_bytes(b"")

        c = Client()
        c.initialize()

        cases = [
            ("*.dll", {"top.dll"}),
            ("**/*.dll", {"top.dll", "bin/Release/net10.0/App.dll"}),
            ("bin/**/*.exe", {"bin/Release/net10.0/App.exe"}),
            ("foo?.txt", {"foo1.txt"}),
            ("nested/path/*.json", {"nested/path/data.json"}),
            ("åäö 日本語.txt", {"åäö 日本語.txt"}),
            ("blob.bin", {"blob.bin"}),
            ("zero.txt", {"zero.txt"}),
            ("logs/*.txt", {"logs/build.txt"}),
        ]
        for pattern, want in cases:
            out = Path(dest) / pattern.replace("/", "_").replace("*", "s").replace("?", "q")
            r = c.call(
                "windows_run",
                {
                    "program": "cmd",
                    "args": ["/c", "ver"],
                    "workspace": ws,
                    "artifacts": [pattern],
                    "artifactsDir": str(out),
                },
            )
            got = {a["path"].replace("\\", "/") for a in r.get("artifacts", [])}
            check(f"artifact {pattern}", got, want)
            if got:
                sized = all(isinstance(a.get("bytes"), int) for a in r["artifacts"])
                if not sized:
                    bad(f"artifact {pattern} metadata", "missing byte counts")

        for bad_pattern in ["../escape.txt", "../../etc/passwd", "C:\\Windows\\System32\\*", "bin/../../x"]:
            r = c.call(
                "windows_run",
                {"program": "cmd", "args": ["/c", "ver"], "workspace": ws, "artifacts": [bad_pattern]},
            )
            check(f"traversal refused: {bad_pattern}", r["isError"], True)

        r = c.call("windows_run", {"program": "cmd", "args": ["/c", "ver"], "artifacts": ["*.dll"]})
        check("artifacts without a workspace are refused", r["isError"], True)
        c.close()
    finally:
        shutil.rmtree(ws, ignore_errors=True)
        shutil.rmtree(dest, ignore_errors=True)


def test_system_tools():
    print("== info and doctor through MCP ==")
    c = Client()
    c.initialize()
    r = c.call("winquick_info")
    check("info succeeds", r["isError"], False)
    check("info reports a version", "version" in r, True)
    check("info reports the desktop capability", "desktop" in r, True)
    check("info is structured, not a terminal dump", isinstance(r.get("capabilities"), list), True)

    r = c.call("winquick_doctor")
    check("doctor succeeds", r["isError"], False)
    check("doctor reports overall health", isinstance(r.get("healthy"), bool), True)
    checks = r.get("checks", [])
    check("doctor returns individual checks", len(checks) > 5, True)
    shaped = all(set(("section", "name", "status", "message")) <= set(x) for x in checks)
    check("every check has name, status and message", shaped, True)
    statuses = {x["status"] for x in checks}
    check("statuses are machine-readable", statuses <= {"ok", "note", "fail"}, True)
    c.close()


def test_desktop_without_session():
    print("== desktop tools with no session ==")
    c = Client()
    c.initialize()
    r = c.call("desktop_status")
    check("desktop_status answers when nothing runs", r["isError"], False)
    if r.get("running"):
        print("  (a session is running; skipping the no-session assertions)")
        c.close()
        return
    check("desktop_status reports not running", r.get("running"), False)

    r = c.call("ui_tree", {"title": "Nothing"})
    check("ui_tree without a session is a tool error", r["isError"], True)
    check(
        "the error points at desktop_start",
        "desktop_start" in r["_content"][0]["text"],
        True,
    )
    r = c.call("desktop_stop")
    check("desktop_stop is idempotent", r["isError"], False)
    check("desktop_stop reports nothing was running", r.get("wasRunning"), False)

    r = c.call("ui_click", {})
    check("ui_click without a selector is refused", r["isError"], True)
    c.close()


def main():
    print(f"MCP tests against {WQ}\n")
    test_lifecycle()
    test_errors()
    test_stdout_is_protocol_only()
    test_clean_shutdown()
    test_desktop_without_session()
    if have_runtime():
        test_system_tools()
        test_windows_run()
        test_workspace_immutability()
        test_artifacts()
    else:
        print("  (skipping Windows-touching tests: no runtime installed)")
    print(f"\n== {PASS} passed, {FAIL} failed ==")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
