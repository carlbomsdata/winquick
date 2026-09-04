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

Twelve packages make the screen work: `COM`, `Windows-Runtime-Metadata`,
`Fonts`, `GDIPlus`, `Graphics`, `Graphics-UXTheme`, `Apps`, `PnP`,
`Driver-Support`, `Connectivity`, `WPF-Support` and `DeveloperTools`.

A desktop also gets everything the `dotnet-framework` capability applies, so a
session can run a .NET Framework application rather than only build one:
`Apps-WOW64`, `COM-WOW64`, `WLAN`, `NetFx45` and `NetFx45-WOW64`. The two
lists overlap and are deduplicated before staging — copying the same read-only
CAB off the mounted ISO twice fails with "Permission denied" before DISM has
run at all. Seventeen packages in total.

Three things about this are not obvious, and each one failed silently before it
was understood.

**`/Online` does not work.** Applying any of these to the running Validation OS
returns `0x80070032` (`ERROR_NOT_SUPPORTED`). Offline servicing of a mounted
image works for every one of them.

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

An option a verb does not understand is also an error. That sounds pedantic
until you mistype `--class-name` for `--class`: the selector silently loses a
term, matches something else, and answers confidently about the wrong control.

`get` reports a combo box's selection as its `value`, even though a
non-editable combo box exposes no value pattern of its own — "which item is
chosen" is the thing people actually want to read.

## Addressing controls in WinForms

WPF gives every named control an AutomationId for free: `x:Name="SaveButton"`
becomes `--automation-id SaveButton`. **WinForms does not.** A WinForms control
exposes an AutomationId only if its `Name` property is set:

```csharp
var save = new Button { Text = "Save", Name = "SaveButton" };   // --automation-id SaveButton
```

Without it, `--automation-id` finds nothing. Two things still work:

* `AccessibleName` surfaces as the UI Automation **Name**, so
  `--name SaveButton --control-type Button` addresses it.
* `--control-type` narrows a name that matches more than one element — a
  WinForms combo box reports both the box and its inner text with the same name,
  so the type is what separates them.

Setting `Name` is the better habit: it is what an automated test will look for,
and it costs one property.

## Capturing one of several identical windows

`--title` matches on substring, so two windows of the same application are
ambiguous and WinQuick refuses to guess. `winquick desktop windows` reports each
window's handle, and `--hwnd` takes it:

```console
$ winquick desktop windows | grep hwnd
$ winquick desktop screenshot one.png --hwnd 131146
$ winquick desktop get --hwnd 131146 --automation-id StatusText
```

`screenshot`, `get`, `tree`, `find` and the interaction verbs all accept it.

## Sessions

```
winquick desktop start --app ./publish
winquick desktop launch 'app\MyApp.exe'
winquick desktop click --automation-id SaveButton
winquick desktop stop
```

A session starts in about **380 ms**, and each verb after that is a round trip of
a few milliseconds over the control disk.

It is not booting Windows in 380 ms. Nothing could. It is restoring a Windows
that already booted.

The session is still disposable. It runs on a copy-on-write overlay over the
desktop image, and `winquick desktop stop` deletes it. `winquick clean` stops a
running session first, so no orphaned QEMU is left behind.

One session at a time. Starting a second reports the first one's pid rather
than quietly racing it.

A running session is a whole virtual machine with four processors and 4 GiB.
It costs real capacity: a `winquick run` issued while a desktop session is up
takes seconds rather than the usual ~300 ms. Stop the session, or expect the
builds you interleave with it to be slow.

## Why a session starts in 380 ms

The first version booted Windows on every `winquick desktop start` and took
9.3 seconds. Profiling said what you would expect:

```
[     0ms] session directory
[    20ms] disk overlay
[    36ms] volumes built
[    43ms] qemu spawned
[  9332ms] guest agent ready (windows booted)
[  9517ms] bridge answering
```

Thirty-four milliseconds of work and nine seconds of watching Windows boot —
the same nine seconds, doing the same things, every single time.

`winquick run` had already solved this for commands: boot once, freeze the
guest with QEMU's migration, and restore RAM and devices per run instead of
booting. A desktop session now does the same, with one difference that matters.
The command state is frozen with the agent waiting for work. The desktop state
is frozen **after the bridge is already answering on the control channel** — a
state frozen at the login prompt would still have to bring up the desktop stack
and the bridge on every restore, which is most of the cost.

```
[    17ms] prepared state validated
[    30ms] volumes cloned
[    30ms] qemu spawned
[   421ms] guest restored
[   516ms] session ready
```

Preparing it costs about 17 seconds, once, on the first start after the
capability is installed or anything about the machine changes.

### The application volume

Freezing a guest that has already mounted its volumes creates one problem. The
snapshot was taken with one application volume attached; the next session has a
different one. Windows is holding a cached directory for contents that have
since been replaced.

So the bridge and the application live on **separate volumes**. The bridge
volume is frozen into the state and never rewritten — `wqui.exe` is executing
from it, and dismounting it would be dismounting the program. The application
volume is a fixed size, refilled per session without reformatting so the
filesystem identity the frozen guest remembers stays valid, and the host asks
the bridge to dismount and remount it once, immediately after restoring. That
is the same trick the guest agent uses on the mailbox, and it costs about 95 ms.

### It is still disposable

More so, if anything. Every session restores the *same* frozen RAM and a fresh
clone of the same frozen disk, so there is no accumulating drift — a session
cannot inherit anything from the one before it, because it does not start from
it. Guest writes land in the session's own overlay and go when it stops.

Measured: after a session that saved records, wrote `C:\dirty.txt`, added a
registry key and grew its overlay to 40 MB, the prepared state was byte for byte
what it had been, and the next session came up with an empty form.

### Sizing

Four processors turned out to buy nothing. Start, application launch, UI
automation and capture are all within noise between two and four, while four
takes twice as much of the host away from whatever else is running — a
`winquick run` issued alongside a four-processor session used to take minutes.
At two it takes 290 ms, the same as with no session at all.

Memory costs twice over: it is the session's resident size *and* most of the
prepared state, which is read back on every start. Halving 4096 MiB to 2048 took
a start from 507 ms to 349 ms and the resident size from 4.6 GiB to 2.3 GiB,
with no effect on anything measurable in the guest.

| | 1 vCPU | 2 vCPU | 4 vCPU |
|---|---|---|---|
| session start | 499 ms | **490 ms** | 533 ms |
| launch + first window | 684 ms | 665 ms | 683 ms |
| five UIA reads | 223 ms | 269 ms | 272 ms |
| screenshot | 133 ms | 134 ms | 156 ms |
| concurrent `winquick run` | — | 341 ms | 312 ms |

The defaults are two processors and 2048 MiB. `--cpus` and `--memory` override
them, and either one changes the machine, so the prepared state is rebuilt.

### Invalidation

A prepared state is a frozen machine, and restoring RAM against a machine it did
not come from is not slightly wrong, it is undefined. So it records everything
that can make it incompatible — WinQuick's version, the mailbox and control
protocols, the desktop image, the guest agent, the guest bridge, the QEMU binary
and version, the firmware, memory, processors, machine type, every capability
volume's identity, and the full device topology. Any difference discards it and
rebuilds. A corrupt or incomplete one is discarded too, rather than run.

## Scripts

`winquick ui-test` runs a file of the same verbs, plus `screenshot`, `sleep`
and `expect`. An `expect` line takes a selector and exactly one assertion:

| Assertion | Checks |
|---|---|
| `--expect-name <text>` | the element's name, exactly |
| `--expect-name-contains <text>` | ...or a substring of it |
| `--expect-value <text>` | its value, exactly |
| `--expect-contains <text>` | ...or a substring of it |
| `--expect-toggle On\|Off` | a check box's state |
| `--expect-enabled true\|false` | whether the control can be used at all |

Asserting against a property the element does not have says so, and suggests
the assertion that would work — a list has no value, so `--expect-contains`
against one reads as an empty string and looks like an application bug rather
than a misaimed test.



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

## What the dogfood changed

The capability was finished, tested and documented before anyone tried to *use*
it. A fresh Claude Code session was then given a small WPF utility with five
planted defects and told only to make it satisfy its requirements file. It found
and fixed all five, kept the two constructs that looked wrong and were not, and
verified the result against the running UI. What it could not do cleanly is what
this section is about — see
[experiments/desktop-dogfood](../experiments/desktop-dogfood/) for the whole
run.

| What went wrong | Fix |
|---|---|
| An option a verb did not understand was ignored. `--class-name` (for `--class`) silently dropped from the selector and the answer came back about a different element. | Unknown options are refused, listing what the verb does understand. |
| `tree --automation-id X` ignored the selector and dumped the whole window. | `tree` scopes to the selected element. |
| Requirement 8 — "Save is disabled until a name is entered" — could not be asserted in a script at all. | `--expect-enabled`. |
| A combo box reported no value, so its selection was only reachable by walking children looking for `selected: true`. | `get` reports the selection as the value. |
| `--expect-contains` on a list compared an empty value and read as an application bug. | Missing properties say so, and suggest the assertion that fits. |
| The assertion list in `--help` ended in `...`; the session resorted to running `strings` on the binary. | All six are listed. |
| A desktop session attached the installed capability volumes directly and Windows wrote to them on mount, so `dotnet-sdk.img` changed underneath. | Sessions clone them, as `winquick run` always has. |
| Roughly one session start in ten never came up, because the bridge scanned for its control disk once, before the disks had all enumerated. | The scan retries, and a failure now reports what the bridge printed instead of only "did not answer". |

The last two are the interesting ones. Neither is a UI problem, and neither
would have been found by testing the feature against itself: they needed
somebody using it for an afternoon without knowing how it works.

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

## On Linux/KVM

The desktop capability works on a Linux host against an x64 guest. Measured on
Ubuntu 24.04 x86_64 / KVM / QEMU 11.1.0 against Validation OS x64:

- `winquick capability install desktop` completes; the guest bridge is built
  as a **PE32+ x86-64** `wqui.exe`, so the architecture follows the guest
- `desktop start`, `status`, `launch`, `wait-window`, `stop` all work
- `desktop tree` returns 33 controls carrying automation IDs
- reading, typing and clicking all take effect. `StatusText` reads `Ready`;
  typing into `NameBox` gives it the value `KVM-WORKS`; clicking `SaveButton`
  leaves `StatusText` reading `Saved: KVM-WORKS / Engineering / basic`
- `ui-test` runs its whole script and its assertions pass
- screenshots are **1280x800 at 100% non-black with over a thousand distinct
  colours**, and the window captures are 620x460 -- the demo's own size
- desktop MCP is **34/34**; teardown leaves no QEMU and no run directory

### Why an x64 desktop uses `-vga std`

It did not work at first: every frame was a single flat black while UI
Automation returned real controls with correct geometry. Three things were
wrong, and only the third mattered.

The media was mounted with `/usr/bin/hdiutil`, which only macOS has, so the
build could not even collect its packages elsewhere. Both discs are now read
without mounting -- the Validation OS media is UDF, the virtio-win disc is
ISO 9660, and WinQuick reads both itself. Then the virtio driver was staged
from the disc's `ARM64` directory whatever the guest was; the disc carries
ARM64 builds regardless, so on x64 the staging *succeeded* and installed a
driver that could never bind.

Fixing both left the screen black, and the measurement that explained it was
QEMU's own `screendump`: the scanout was **640x480 and 99.8% black** while
Windows reported a 1024x768 desktop. The guest was drawing somewhere the
scanout could not see, because `viogpudo` had still not bound.

The device was the problem rather than the driver. The aarch64 `virt` machine
has no VGA, so a virtio GPU is the only option there and `viogpudo` is
genuinely required. x86_64 has one, and `-vga std` is the Bochs-style adapter
Windows drives with its own inbox Basic Display Adapter -- no third-party
driver, no INF staging, nothing to bind and nothing to go wrong. WinQuick
never required virtio-gpu; it requires a desktop that renders.

The display is part of `desktop_device_signature`, so a session state frozen
against one display is never restored against another.
