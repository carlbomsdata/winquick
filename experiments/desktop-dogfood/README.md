# Desktop dogfood

The question this experiment answers:

> Can a genuinely fresh Claude Code session use WinQuick to develop, visually
> inspect, interact with, debug and verify a real WPF application on a Mac?

Not "does the screenshot API return bytes". Whether an agent that has never seen
WinQuick can pick it up from `--help`, find a bug it could not have found by
reading the source, fix it, and prove the fix against a real Windows UI.

## The application

[`device-config`](../../../device-config) — a separate repository, so WinQuick is
not testing itself. A "Device Configuration" utility: a header, a device-name
`TextBox`, a mode `ComboBox`, an *Enable logging* `CheckBox`, a `Save` button, a
saved-devices `ListBox` and a status line. `requirements.md` describes what it
must do in terms of what a person sees and does.

## The defects

Five, planted before the session, spanning four categories. None is a compiler
error; the application builds cleanly and its source reads plausibly.

| # | Category | Defect | Requirement broken |
|---|---|---|---|
| D1 | Visual | `SaveButton` has `Margin="0,-34,0,0"`, which lifts it exactly over the *Enable logging* checkbox. The checkbox is present in the UI tree and completely invisible on screen. | 14 (and 6 in practice) |
| D2 | Interaction | `UpdateSaveState()` sets `IsEnabled = true` unconditionally, so Save is live with an empty name. | 8, 9 |
| D3 | Interaction | `Save_Click` hard-codes `logging = "logging off"` and never reads `LoggingCheck`. | 10 |
| D4 | State | `Save_Click` reports `_savedName` — the *previous* save — and only then assigns the current name. The first save shows an empty name; every later one lags by one. | 11 |
| D5 | Automation | The status line carries `AutomationProperties.AutomationId="StatusLabel"`, but the contract says `StatusText`. A semantic selector for it fails outright. | 15 |

D1 is only findable in a screenshot: the checkbox reports sensible bounds and
`offscreen: false`, because as far as UI Automation is concerned it is laid out
normally — it is simply painted underneath the button.

D3 and D4 need interaction: the source looks reasonable until you toggle the box
and watch what Save records.

## The deliberately suspicious, deliberately correct parts

Two constructs look like they want deleting and must survive:

- `Loaded += (_, __) => UpdateSaveState();` in the constructor. It reads as
  redundant next to the `TextChanged` handler. It is not: the Save button has no
  `IsEnabled` in XAML, so this is what establishes the initial state. Deleting it
  breaks requirement 8 in a way only a running UI shows.
- `NormalizeName`, a regex that collapses internal whitespace. It looks like
  over-engineering; requirement 7 asks for exactly it.

Verified correct against the running application before the session:
`"  PLC  02 "` is saved as `PLC 02`.

## Baseline, measured through the public CLI

`baseline/` holds what the broken application actually does. Every command was a
public `winquick` verb; nothing reached past the product.

```
LoggingCheck bounds  x 488 y 365  w 424 h 15    offscreen: false
SaveButton   bounds  x 488 y 363  w 120 h 32    <- same pixels
SaveButton   enabled: true        with the device name empty

type PLC-01 / mode Diagnostic / logging ON / Save
  status  = 'Saved:  (Diagnostic, logging off)'
  history = [' — Diagnostic — logging off']
Save again, same values
  status  = 'Saved: PLC-01 (Diagnostic, logging off)'

winquick desktop get --automation-id StatusText
  error: no element matches automation-id=StatusText
```

Requirement 10 wants `Saved: PLC-01 (Diagnostic, logging on)`.

| File | What it is |
|---|---|
| `baseline/01-initial.png` | the window at rest — the checkbox is missing |
| `baseline/02-after-save.png` | after the interaction above |
| `baseline/tree-before.json` | the UI Automation tree, broken state |
| `baseline/interaction-before.txt` | the transcript above |

## The session

Prompt, in full:

> You are working on a Mac, in the repository at
> ~/source/repos/device-config
>
> Task: Fix this Windows application so it satisfies requirements.md. Build it
> and verify the actual Windows UI and behaviour before you finish.
>
> Use the tools available in the environment. Do not modify anything outside
> this repository.
>
> When you are finished, report: what you changed and why; how you verified each
> requirement (say which ones you checked by looking at the running UI, and
> which by reading code); the exact list of shell commands you ran, in order;
> anything about the tooling that was confusing, missing, or that you got wrong
> at first.

Nothing about QEMU, images, servicing, control channels or how any of it works.
The repository's `CLAUDE.md` says only that `winquick run` exists for Windows
commands and that `winquick desktop --help` and `winquick ui-test --help` exist
for desktop testing.

### Metrics

| | |
|---|---|
| Wall-clock | 7 min 49 s |
| Tool calls | 41 |
| WinQuick invocations | ~45 |
| Builds | 2 (`dotnet build`, then `dotnet publish`) |
| Desktop sessions started | 3 (two interactive, one via `ui-test`) |
| Screenshots taken | 4 |
| UI tree reads | 4 |
| Semantic actions (`type`/`click`/`select`/`toggle`) | ~18 |
| Raw coordinate/key actions | 0 |
| Code-edit iterations | 1 edit pass, then verify |
| Human interventions | 0 |
| Requirements passing at the end | 15 / 15 |

### Discovery

It ran `winquick --help`, then `winquick ui-test --help` and
`winquick desktop --help`, then `winquick doctor` and `winquick info` — before
touching the application. It went `winquick run -- dotnet build` →
`winquick run -a "publish/**" -- dotnet publish` → `desktop start --app` →
`launch` → `wait-window` → `screenshot` → `tree` → `get` → interact → `stop` →
`ui-test`, and finished by writing a `smoke.uitest` so the verification was
repeatable. It used `AutomationId` throughout and never once resorted to
coordinates.

The one thing it could not find in `--help` was the list of `expect`
assertions, which the help text truncated with `...`. It ran `strings` on the
binary to get them. That is now fixed.

### What it fixed

All five, minimally:

- **Visual (D1)** — removed `Margin="0,-34,0,0"` from the Save button. It
  confirmed the fix from element bounds *and* the screenshot: checkbox
  y=365–380, Save y=396–428, list y=446–576.
- **Interaction (D2)** — `UpdateSaveState()` now derives from the text, reusing
  `NormalizeName` so whitespace-only names are covered by construction.
- **Interaction (D3)** — reads `LoggingCheck.IsChecked`.
- **State (D4)** — reports `name`, not the previous save; deleted the now-unread
  `_savedName` field.
- **Automation (D5)** — `StatusLabel` → `StatusText`.

### The traps held

Both suspicious-but-correct constructs survived. `NormalizeName` was not only
kept but *reused* for the enabled check. The `Loaded` handler was kept, and the
session added `IsEnabled="False"` in XAML alongside it so the initial state no
longer depends on the handler firing — a strictly better arrangement than the
one it was given.

No speculative changes anywhere.

## The comparison

The same broken application, a fresh session, and no Windows of any kind — no
machine, no VM, no emulator. Same task.

**It also fixed all five defects.** Its output, built and run through WinQuick
afterwards, passes the same 33-step requirements script. That is the honest
result and it is worth stating plainly: a careful reader can find these bugs by
reading. The claim "an agent cannot reason about WPF without WinQuick" is false,
and this experiment does not support it.

What it could not do is *know*. Four differences, all of which it reported
itself:

**It could not check the visual requirement.** Asked to rate each requirement
verified-or-inferred, it marked requirement 3 "INFER only, weakest" and said of
requirement 14: *"'fully visible, nothing clipped' is the one claim I would want
a screenshot for."* It had hand-summed row heights — ~302 px of content in a
~431 px client area — and reasoned the button must clear the checkbox. It was
right. It had no way to find out.

**It restructured the application to compensate.** It extracted the four pure
decisions — normalise, can-save, status text, history row — into a new
`DeviceForm.cs`, explicitly *"the only way to execute any of this behaviour
without Windows"*. That is a real cost. The architecture changed to serve the
absence of a test environment, not the requirements. The WinQuick session made
no structural change at all, because it could just run the thing.

**It made changes outside the requirements.** `MinHeight`/`MinWidth` on the
window against a clipping risk it could not observe, and a rewrite of the crash
log path in `App.xaml.cs`. Both defensible; neither asked for; neither
verifiable from where it stood.

**It carried uncertainties that cost 22 ms to resolve.** It flagged that
`HeaderText` and `StatusText` are `TextBlock`s and that it could not confirm UI
Automation would surface them — so requirements 1 and 13 might be unreachable in
practice. It flagged that `ModeCombo.SelectedItem as ComboBoxItem` might fail at
runtime and the `?? "Standard"` fallback would then silently report the wrong
mode. The WinQuick session never wondered about either: `get --automation-id
StatusText` answers in 22 ms, and `select --item Diagnostic` followed by reading
the status settles the cast.

Its own summary of its position: *"'VERIFIED' below never means 'I saw the app
do it'."*

That is the difference the desktop capability buys. Not the ability to write the
fix — the ability to stop guessing about it.
