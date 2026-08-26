# WinQuick as an MCP server

WinQuick speaks the [Model Context Protocol](https://modelcontextprotocol.io)
natively. An AI agent can build, run and test Windows software, drive a real
WPF or WinForms application through UI Automation, and look at what Windows is
actually rendering — without knowing any WinQuick CLI syntax.

```console
brew install Carlboms-Data-AB/tap/winquick
winquick setup
claude mcp add winquick -- winquick mcp
```

That is the whole setup. Claude Code starts `winquick mcp` as a child process
and talks to it over stdin/stdout.

## How it fits together

`mcp` is a mode of the same binary, not a separate program:

```
  CLI ─┐
       ├──> runner / desktop / facts ──> QEMU + Windows
  MCP ─┘
```

The MCP server calls the same internal functions the CLI calls. It does not
spawn `winquick` and parse its output, so there is nothing to drift and no
terminal formatting in the way. There is no daemon: when the client closes the
connection, the process ends.

## Configuration

For Claude Code:

```console
claude mcp add winquick -- winquick mcp
```

For any other MCP client, the generic form is:

```json
{
  "mcpServers": {
    "winquick": {
      "command": "winquick",
      "args": ["mcp"]
    }
  }
}
```

Transport is **stdio only** in this release. There is no HTTP transport, no
remote mode, no authentication and no listening socket.

## Tools

### Running things

| Tool | What it is for |
|---|---|
| `windows_run` | Run one command in a disposable Windows and get stdout, stderr and the exit code back |

`windows_run` is the tool for almost everything: builds, tests, PowerShell,
any Windows executable. Arguments are an **array**, never a shell string —
WinQuick applies the correct quoting for `cmd` and for native programs itself.

```json
{
  "program": "dotnet",
  "args": ["test", "MyProject.sln"],
  "workspace": "/Users/me/code/myproject",
  "artifacts": ["**/TestResults/**/*"]
}
```

`workspace` is an absolute host path. It appears inside Windows as
`C:\workspace` and is the working directory. It is **copied in and never
copied back**, so a build cannot change your source; ask for output explicitly
with `artifacts`.

The result:

```json
{
  "exitCode": 0,
  "stdout": "...",
  "stderr": "",
  "durationMs": 287,
  "command": "dotnet test MyProject.sln",
  "artifacts": [
    { "path": "TestResults/report.trx", "hostPath": "/…/winquick-artifacts/…", "bytes": 4211 }
  ],
  "artifactsDir": "/…/winquick-artifacts"
}
```

A non-zero `exitCode` is a normal result, not a tool failure: `dotnet test`
returning 1 means tests failed, and the agent needs to read that rather than
see a transport error.

Artifact patterns are a real glob subset: `**/*.dll`, `bin/**/*.exe`,
`logs/*.txt`, `foo?.txt`, `bin/Release/**`, or a named file. A single `*`
matches one directory level; `**` recurses. Patterns containing `..` or an
absolute path are refused before the run starts.

### Desktop lifecycle

| Tool | What it is for |
|---|---|
| `desktop_start` | Boot a real Windows desktop and leave it running |
| `desktop_stop` | Shut it down and delete its disposable disk |
| `desktop_status` | Whether a session is running, its pid and uptime |
| `desktop_launch` | Start a program inside the session |
| `desktop_wait_window` | Wait for a window and return its handle |

There is **one desktop session at a time**. Calling `desktop_start` while one is
running returns the existing session (`alreadyRunning: true`) rather than
creating a second. The desktop capability must be installed first:
`winquick capability install desktop`.

`desktop_wait_window` returns `hwnd`, `title` and `pid` at the top level, plus
the full window object. Launching returns as soon as the process starts, which
is before its window exists — always wait before inspecting.

### UI Automation

| Tool | What it is for |
|---|---|
| `ui_tree` | The UI Automation tree of a window, as compact JSON |
| `ui_get` | Read one element: name, value, control type, enabled |
| `ui_click` | Click an element, addressed semantically |
| `ui_type` | Type text into an element |
| `ui_screenshot` | A real PNG of what Windows is rendering |

Elements are addressed by `automationId` first, then `name` with
`controlType`, `className`, and scoped with `title` or `hwnd`. A selector that
matches more than one element is an error listing the candidates rather than a
guess.

**WPF** derives an `AutomationId` from `x:Name` automatically. **WinForms does
not**: it only exposes one where `Control.Name` was set in code. For WinForms
controls without a name, `name` plus `controlType` is the reliable selector.

```json
{ "automationId": "SaveButton", "title": "Device Configuration" }
{ "name": "Save", "controlType": "Button", "title": "Device Configuration" }
```

### Screenshots

`ui_screenshot` returns the PNG **in the response**, as an MCP `image` content
block with `mimeType: image/png` and base64 data, alongside a text block giving
width, height, byte count and the non-black fraction. The agent does not have
to open a file.

The capture is taken inside Windows, so it shows the real composited desktop.
QEMU's own framebuffer is blank on this platform and is never used. Pass
`title` or `hwnd` to frame a single window — `hwnd` is also how you
disambiguate two windows sharing a title.

### System

| Tool | What it is for |
|---|---|
| `winquick_info` | What is installed: version, runtime, capabilities, desktop state |
| `winquick_doctor` | Structured checks plus a list of problems to fix |

`winquick_doctor` returns each check as `{section, name, status, message}` with
status `ok`, `note` or `fail`, plus an overall `healthy` flag and a `problems`
array. A `note` is context, not a fault — "no prepared guest yet" is normal on
a fresh install.

## Semantics worth knowing

**Disposable execution.** Every `windows_run` starts from a pristine Windows
image and throws the environment away afterwards. Files, registry keys and
environment variables written by one run are gone in the next. The base image
is never modified.

**A persistent MCP process is not a persistent VM.** The server stays alive for
the length of the conversation; Windows does not. A VM exists only during a
`windows_run`, or between `desktop_start` and `desktop_stop`.

**No network.** The guest has no network adapter. This is deliberate and there
is no option to enable it. Use `winquick cache sync` for offline NuGet
restores.

**The host workspace is never mutated.** The guest gets a copy. Files it
creates, changes or deletes do not reach the Mac. `artifacts` is the only way
out.

## Errors

Three different things are reported three different ways:

| What happened | How it appears |
|---|---|
| Malformed JSON, unknown method, unknown tool, bad `tools/call` params | JSON-RPC error (`-32700`, `-32601`, `-32602`) |
| The tool ran but could not do the job — no desktop session, selector not found, timeout, capability missing, invalid artifact pattern | A tool **result** with `isError: true` and a readable reason |
| A Windows program exited non-zero | A **successful** tool result whose `exitCode` says so |

The middle case matters most: an agent should be able to read "no desktop
session is running — call desktop_start first" and act on it, rather than
concluding the server is broken.

## Lifecycle and cleanup

- Closing the connection (stdin EOF) ends the server.
- A desktop session **that the MCP server started** is stopped when the server
  exits, so no Windows VM outlives the conversation that asked for it. A
  session you started yourself from the CLI is left alone.
- `Ctrl-C` (SIGINT) aborts a run in progress, cleans up its VM, and exits 130.
- `SIGKILL` cannot be caught by any process, so a desktop session started over
  MCP can survive it. `winquick desktop stop` reclaims it.
- Nothing is written to stdout except protocol traffic. The server takes the
  real stdout for itself at startup and redirects everything else to stderr, so
  no log line from any part of WinQuick can corrupt the connection.

## Known limitations

- **stdio transport only.** No HTTP, no remote MCP, no authentication.
- **One desktop session**, one client.
- **No output streaming**: `windows_run` returns when the command finishes.
- Output above 512 KiB per stream is truncated in the middle, keeping the
  beginning and the end, and says so with `stdoutTruncated` and the original
  byte count. It is never silently cut.
- **Cancellation is acknowledged but not acted on.** WinQuick's operations are
  synchronous; `notifications/cancelled` is accepted and ignored rather than
  interrupting a run in progress.
- Apple Silicon macOS only, as with the rest of WinQuick.

## Implementation note

The JSON-RPC layer is written directly against `serde_json` rather than taken
from an SDK. WinQuick is a synchronous binary with six dependencies, and the
official Rust MCP SDK brings an async runtime with it; the subset a stdio tool
server needs is small enough that owning it costs less than the dependency
would, and keeps `winquick` a single self-contained executable with no Node,
Python or npm anywhere in the picture.
