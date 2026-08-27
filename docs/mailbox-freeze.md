# Freezing a guest half a step too early

The prepared guest is the whole point of WinQuick: capture Windows once, restore
it for every later command. On Windows the restored guest resumed and executed
real code, and then ignored the command it was given. This is what that was.

**Status: fixed.** Measured on ROAD-WARRIOR01 (Windows 11 Pro 25H2 build 26200,
i5-8265U, WHPX, QEMU 11.1.0 with
[`patches/whpx-stop-and-copy.patch`](../patches/whpx-stop-and-copy.patch)),
Validation OS x64 26100.8972, `-cpu Nehalem`, **one vCPU** — multiprocessor
restore is a separate, unsolved problem described in
[whpx-resume.md](whpx-resume.md).

| | warm runs from a freshly prepared guest |
|---|---|
| before | **0 of 8** |
| after | **8 of 8**, then **20 of 20** |

With the fix, from one immutable prepared guest, twenty consecutive warm
`cmd /c ver` runs:

| | min | p50 | mean | p95 | max |
|---|---|---|---|---|---|
| state restore | 92 ms | 103 ms | 113 ms | 159 ms | 192 ms |
| command observed by the guest | 384 ms | 517 ms | 541 ms | 772 ms | 829 ms |
| full `winquick run` roundtrip | 8.6 s | 10.4 s | 12.9 s | 19.4 s | 34.3 s |

The prepared state and the canonical image were byte-identical afterwards, and
no QEMU process was left behind. The roundtrip is dominated by copying the
workspace and artifact volumes per run, not by anything measured here; that is
the next thing worth attacking, and it is not this.

## The wrong answer, and why it was wrong

The obvious explanation was cache coherency: the host writes a command into a
FAT volume the frozen guest already has mounted, so the guest keeps reading its
own stale copy. The agent already guards against exactly that — it dismounts and
remounts the mailbox on every poll, which is what makes the same design work on
macOS.

Two measurements killed that theory:

- The mailbox on disk during a failed run was **correct**. `WQCMD.CMD` held the
  right command and `WQGO.TXT` the right token. There was nothing stale to read.
- A prepared mailbox sometimes carried a directory entry owning a cluster the
  FAT still called free, so the host's allocator handed the same cluster to
  `WQCMD.CMD` and the two files overlapped. That is real, and it is a symptom of
  the same root cause — but it was not the blocker, because the command file was
  the one that survived intact. Clearing the stale flag fixed the overlap and
  changed the success rate not at all: still 0 of 8.

## The actual answer

Instrument the agent to append a line to the mailbox on every one of its first
thirty poll iterations, then look at a failed warm run:

```
--- the warm attempt's mailbox
  WQMARK.TXT   winquick
  WQCMD.CMD    @echo off / cmd /c ver
  WQGO.TXT     n1b1418cfb7c37744c390
  (no WQDIAG.TXT)
```

**Zero poll iterations.** Not one, in forty seconds. The restored guest was
executing — RIP advances through kernel and user code — but the agent never got
back to its loop.

The prepared guest's own mailbox says why: it contains `WQMARK.TXT` and
`WQREADY.TXT` and nothing else. No poll iteration had completed there either.
WinQuick waits for `WQREADY.TXT` and stops the guest the instant it appears —
and the agent writes that flag and *then* dismounts the volume:

```
>%WQ%\WQREADY.TXT echo 1
mountvol %WQ% /P          <- the guest is frozen somewhere in here
:wait
  ...
```

The flag becomes visible to the host the moment its directory entry reaches the
image, which is the middle of the agent's work, not the end of it. So the
prepared state captured a guest with mailbox I/O still in flight. Restored into
a fresh QEMU process, against a fresh copy of the image, that operation never
completes: the agent stays blocked in the dismount and never reaches the poll
loop that would notice the next command.

That also explains the overlapping clusters — a volume frozen mid-dismount has a
directory entry on disk whose allocation is still only in Windows' cache — and
it explains why this was maddeningly intermittent. One prepared state, built
when the freeze happened to land on a quiet moment, produced six good warm runs
in a row. Almost every other one produced none.

## The fix

Wait for the guest to go quiet before taking its picture:

```rust
wait_for(&mbox, mailbox::READY, &mut child, deadline)?;
std::thread::sleep(SETTLE_BEFORE_FREEZE);
q.stop()?;
```

One and a half seconds, paid once per prepared guest and never per run. It is
long enough for the agent to finish the dismount and get back to its loop, which
is the only state worth capturing.

It is a sleep, which is usually the wrong shape for a fix, so it is worth being
precise about what it is not. It is not a retry, and it is not a wait for
something that might never happen: the agent reaches its idle loop within
milliseconds, and the settle is simply the host declining to photograph it
mid-stride. A tighter version would have the guest signal that it is idle, but
the agent cannot write that signal — being idle means having dismounted the only
volume it can write to.

## What this cost, and the lesson

Three wrong turns, all from reasoning ahead of measurement:

- Cache coherency was assumed because the symptom matched a documented hazard.
  The disk contents disproved it in one look, which should have been the first
  look.
- An accidental "fix" — instrumenting the agent — appeared to work six times out
  of six. It was a different prepared state, not a different agent. A control
  run with the original agent under the same conditions was the thing that
  settled it, and it should have come first.
- The first control was itself invalid: `winquick setup` ends with a smoke test
  that runs the guest, so it had already consumed the one warm attempt the test
  was measuring. Every number from that harness had to be thrown away.

The measurement that mattered took ten minutes once it was aimed correctly: make
the guest write down what it sees, then read it.

## Reproducing

The failure needs a freshly prepared guest each time, because a good state stays
good:

```console
> winquick setup --force --from <ValidationOS.vhdx>
> rm ~/.winquick/restore-unsupported; rm -r ~/.winquick/states
> winquick run --cpus 1 --verbose -- cmd /c ver
```

Look for `warm run` rather than `cold run` in the verbose output, and repeat
from the `rm` each time. Reverting the settle turns 8 of 8 back into 0 of 8.
