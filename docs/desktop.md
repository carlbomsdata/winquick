# The desktop capability

WinQuick's base runtime has no graphics at all. The desktop capability adds a
real Windows desktop — window manager, DWM compositing, WPF and WinForms, UI
Automation — so a coding agent on a Mac can build a Windows GUI application,
watch it draw, read its control tree, click its buttons and check what changed.

It is optional. Installing it does not touch the base image, and
`winquick run` stays exactly as fast as it was.

```
winquick capability install desktop        # once, about a minute
winquick ui-test MyApp.csproj --script my.uitest --out ./shots
```

## What it actually does

```
┌──────────────────────────── macOS ────────────────────────────┐
│  winquick desktop <verb>                                      │
│        │ argv as JSON, written to the control disk            │
│        ▼                                                      │
│  ┌───────────── QEMU (long-lived, headless) ─────────────┐    │
│  │  Windows (serviced Validation OS)                     │    │
│  │    wqui.exe serve ── polls the raw control disk       │    │
│  │        │                                              │    │
│  │        ├── UI Automation  (read the tree, click, type)│    │
│  │        ├── SendInput      (keyboard and mouse)        │    │
│  │        └── GDI capture    (screen or one window)      │    │
│  │    virtio-gpu ──► DWM ──► your WPF window             │    │
│  └───────────────────────────────────────────────────────┘    │
│        ▲ JSON, and PNGs inline, come back the same way        │
└───────────────────────────────────────────────────────────────┘
```

There is no window on the Mac, no RDP, no VNC client and no network in the
guest.

The channel is a disk with no partition table and no filesystem on it. That
sounds odd until you try the obvious thing. `winquick run` hands a command to
the guest through a FAT volume, which is perfect for one command per boot: the
host writes before QEMU starts and reads after the guest has dismounted. A live
session breaks that assumption, because the host writes while Windows still has
the volume mounted. Two independent FAT implementations then hold conflicting
views of the same allocation tables — Windows flushes its cached copy on
dismount, the host writes over it — and the volume ends up genuinely corrupt.
Measured over 60 calls that was a 10% failure rate, and once the tables were
damaged the session stopped answering at all:

```
$ mdir -i mailbox.img ::WQGO.TXT
Fat problem while decoding 125 0
Streamcache allocation problem:: 3
```

A partitionless disk is never mounted by Windows, so it is never cached by
Windows either. Both sides read and write whole sectors at fixed offsets,
payload first and header last, and a 512-byte sector write is atomic at the
device — so a header the other side can read always refers to a payload that is
already there. That is the entire synchronisation story. The same test is now
200 out of 200 with every reply's contents checked, at roughly 8 ms a call
instead of 166 ms.

The mailbox is still used exactly once per session, before anything else touches
it: to tell the agent to start `wqui serve`. The agent then blocks inside it and
never polls that volume again.

## Building the image

Validation OS ships deliberately minimal. The pieces a desktop needs are on
Microsoft's own media as optional CAB packages, and the supported way to apply
them is DISM — which only runs on Windows.

WinQuick's answer is to run DISM *inside WinQuick*. The existing Windows
runtime boots with a copy of its own disk attached as a second device and
services that copy offline. No Windows machine is involved, nothing is
downloaded from Microsoft, and no Microsoft-licensed bytes are redistributed:
the CABs come from the ISO the user already supplied to `winquick setup`.

Twelve packages are applied: `COM`, `Windows-Runtime-Metadata`, `Fonts`,
`GDIPlus`, `Graphics`, `Graphics-UXTheme`, `Apps`, `PnP`, `Driver-Support`,
`Connectivity`, `WPF-Support` and `DeveloperTools`.

Three things about this are not obvious, and each one failed silently before it
was understood.

**`/Online` does not work.** Applying any of these to the running Validation OS
returns `0x80070032` (`ERROR_NOT_SUPPORTED`). Offline servicing of a mounted
image works for all twelve.

**The copy needs its own disk identity.** Windows will not mount two disks with
the same GPT disk GUID and partition GUIDs read-write. It mounts the duplicate
read-only and *discards writes to it without reporting an error*, so DISM
reports success and changes nothing. `src/gpt.rs` gives the copy fresh GUIDs
first.

**The identity has to be put back.** The bootloader records the partition GUID
it boots from, so an image left with fresh GUIDs fails to boot with
`0xc000000e \windows\system32\boot\winload.efi`. The original tables are
snapshotted before servicing and restored after.

## The display adapter

Validation OS has the `BasicDisplay` *service* registered but not
`BasicDisplay.sys` — the file is in none of the CABs, verified by unpacking
every one of them. So there is no inbox driver for a plain framebuffer, and
`ramfb` (what `winquick run` uses) stays black no matter what.

WinQuick therefore attaches a VirtIO GPU and stages Red Hat's `viogpudo`
display-only driver, from the virtio-win ISO, into the image's DriverStore with
`dism /Add-Driver`.

That is the whole driver installation. An earlier version of this code also
hand-wrote a service entry and CriticalDeviceDatabase entries, on the theory
that Validation OS lacks the user-mode PnP needed to finish a driver install.
Inspecting the serviced registry hive afterwards showed otherwise: the device
was bound to DISM's own service (`VioGpuDod`, from the INF's `AddService`
directive), with a `Control\Class\{4d36e968-…}\0000` software key that PnP had
built on first boot, carrying values the driver itself writes at StartDevice
(`HardwareInformation.ChipType = "QEMU VIRTIO GPU"`). The hand-written entries
were unused. Building a fresh image with `/Add-Driver` and nothing else
produced an identical, fully working adapter. `DISM is sufficient` is a load-
bearing fact here: there is no INF parsing or offline registry surgery in
WinQuick, because none is needed.

Inside the guest, Windows reports:

```
\\.\DISPLAY2   Red Hat VirtIO GPU DOD controller
               attachedToDesktop: true   primary: true   1280x800x32
```

`winquick desktop display` prints this, which is the quickest way to tell a
graphics problem from an application problem.

## Screenshots come from inside the guest

`winquick desktop screenshot` captures with GDI inside Windows and returns the
PNG inline over the control channel. It is a real capture of the real composited
desktop, and it can capture a single window as well as the whole screen.

The more obvious design — QMP `screendump`, reading QEMU's framebuffer — is
still available as `--host`, but it is not the default because on this stack it
does not show the desktop. This was measured rather than assumed. Tracing QEMU
during a session shows the guest doing everything right:

```
virtio_gpu_cmd_res_create_2d   res 0x1, fmt 0x2, w 1280, h 800
virtio_gpu_cmd_set_scanout     id 0, res 0x1, w 1280, h 800
virtio_gpu_cmd_res_xfer_toh_2d res 0x1          (×643)
virtio_gpu_cmd_res_flush       res 0x1, w 993, h 519, x 26, y 26
```

Scanout set, hundreds of transfer-and-flush pairs, dirty rectangles that track
the window being moved and redrawn — and no guest errors logged. Yet the host
framebuffer contains only a 19×18 blob at the exact centre of the screen: the
software-drawn mouse cursor that `viogpudo` renders itself (`HWCursor = 0`).
The desktop blit never reaches the buffer the driver transfers.

For comparison, a scanout that was never set dumps at 640×480 with QEMU's
"display output is not active" placeholder, so the geometry alone proves the
guest is driving the device.

Setting `UsePhysicalMemory = 1` changes the failure rather than fixing it: the
cursor disappears too and the host buffer goes entirely black, while the guest
keeps rendering perfectly. The remaining gap is inside `viogpudo`'s
present path under QEMU/HVF on ARM64, which is not something WinQuick can
correct from the outside.

None of this affects what the capability is for. Windows renders correctly —
`winquick desktop screenshot` returns a 1280×800 PNG at 99.97% non-black with
~1000 distinct colours — and the guest-side capture is the better interface
anyway, because it can frame a single window.

## The bridge

`wqui.exe` is a small .NET program built from `guest/wqui/` **inside Windows**
during `winquick capability install desktop`. It is framework-dependent on
purpose: the .NET SDK capability volume already carries
`Microsoft.WindowsDesktop.App`, and a self-contained publish would need runtime
packs the offline guest cannot download.

That is also why the desktop capability requires `dotnet-sdk`.

Every invocation runs one verb and prints one JSON object, so a failure is
never mistaken for an empty result:

| Verb | What it does |
|---|---|
| `windows` | every top-level window: handle, title, class, pid, bounds |
| `display` | adapters, modes, session id — the graphics health check |
| `launch` | start a program |
| `wait-window` | block until a window with this title exists |
| `focus` | bring a window to the foreground |
| `tree` | the UI Automation tree as JSON |
| `find` / `get` | locate elements, read their properties |
| `click` | invoke through the best control pattern, or a real mouse click |
| `type` | set a value, or type into the focused control |
| `key` | key chords such as `ctrl+s`, `alt+F4`, `Enter` |
| `select` | choose an item in a combo box or list |
| `toggle` | check boxes, with `--state on\|off` |
| `mouse` | click or move at a screen coordinate |
| `screenshot` | GDI capture of the screen, a window or a rectangle |

Elements are addressed by `--automation-id` first, then `--name`, `--class` or
`--control-type`. **A selector that matches more than one element is an error**,
listing the candidates, rather than a guess — clicking an arbitrary one of two
buttons is the kind of failure that looks like success in a log.

## Sessions

```
winquick desktop start --app ./publish
winquick desktop launch app\MyApp.exe
winquick desktop click --automation-id SaveButton
winquick desktop stop
```

The guest boots once (about 10 seconds) and stays up; each verb after that is a
round trip of a few milliseconds over the control disk. That ratio is what makes
iterating on a UI bearable.

The session is still disposable. It runs on a copy-on-write overlay over the
desktop image, and `winquick desktop stop` deletes it. `winquick clean` stops a
running session first, so no orphaned QEMU is left behind.

One session at a time. Starting a second reports the first one's pid rather
than quietly racing it.

## Scripts

`winquick ui-test` runs a file of the same verbs, plus `screenshot`, `sleep`
and `expect`:

```
launch app\DemoApp.exe
wait-window --title "WinQuick Demo"
expect --automation-id StatusText --expect-name "Ready"
screenshot 01-before.png --title "WinQuick Demo"

type --automation-id NameBox --text "Tobias Carlbom"
select --automation-id DeptCombo --item Design
toggle --automation-id AdvancedCheck --state on
click --automation-id SaveButton

expect --automation-id StatusText --expect-name "Saved: Tobias Carlbom / Design / advanced"
screenshot 02-after.png --title "WinQuick Demo"
```

Given a `.csproj`, `ui-test` builds it inside Windows first, so no .NET SDK is
needed on the Mac. Given a directory, it takes it as already published.

`examples/WpfDemo` is a real WPF application with a `TextBlock`, `TextBox`,
`ComboBox`, `CheckBox`, `Button` and `ListBox`, and `demo.uitest` drives all of
them.

## Security

The desktop capability gives the guest synthetic keyboard and mouse input and a
way to read any window's contents. That is what makes UI automation possible,
and it is worth being explicit about what it is not:

* **This is not a hardened malware-analysis environment.** The isolation is
  QEMU's, and it is the same as for `winquick run` — no network, a disposable
  disk, no host filesystem access beyond the volumes WinQuick attaches. But
  WinQuick makes no attempt to resist a guest that is actively trying to escape,
  and adding input injection and screen capture does not change that either way.
* **No ports are opened.** The QMP socket is a unix socket inside
  `~/.winquick/desktop/`. Nothing listens on TCP; there is no VNC or RDP server.
* **Screenshots contain whatever is on the guest's screen.** They are written
  where you asked, on your Mac. Nothing is uploaded anywhere.
* **The desktop image is derived from your Microsoft media** and stays under
  `~/.winquick/images/desktop-arm64/`. Like the base image, it must not be
  redistributed.

## Troubleshooting

`winquick desktop display` first — it separates "Windows has no working
display adapter" from "the application did not start".

| Symptom | Likely cause |
|---|---|
| `desktop volume not attached` | the session was started without `--app`, or the volume failed to build |
| `no element matches …` | the control has no `AutomationId`; check `winquick desktop tree` |
| `N elements match …` | narrow the selector; the error lists the candidates |
| window never appears | the app crashed at startup — see `C:\wqcrash.txt` via a `launch` of `cmd /c type C:\wqcrash.txt` |
| `attachedToDesktop: false` on every adapter | the image was built without the virtio driver; reinstall with `--force` |
| session will not start | `~/.winquick/desktop/session.log` has the guest's console output |
| verbs time out after the first one works | the control disk is not readable both ways; it must be attached `cache=writethrough`, and the guest must read it unbuffered |
| a verb returns another verb's answer | the request sequence restarted; it is read back from the disk so it survives one CLI process ending |

The WPF example logs unhandled exceptions to `C:\wqcrash.txt`, which is how the
missing `UIAutomationCore.dll` was found in the first place — the window was
created and then died with a `DllNotFoundException` that nothing else reported.
