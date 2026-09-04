# Security model

What WinQuick does and does not protect you from, stated precisely.

## What this is

WinQuick runs code inside a real Windows guest under a hardware hypervisor
(Apple's Hypervisor Framework, via QEMU). The guest is a separate operating
system on separate virtual hardware; it does not share a kernel with macOS.

**This is a strong isolation boundary, but WinQuick is not a hardened malware
sandbox and is not audited as one.** It is designed for running *your* builds and
tests, and for giving an automated agent somewhere safe to be wrong. It is not
designed for detonating hostile samples. If you need that, use something built
and audited for it.

## When WinQuick uses the network

The *guest* never does: no NIC is attached to it, ever. The host side reaches
the network only when you ask it to, and only to fetch software:

| Command | What it downloads |
|---|---|
| `winquick setup --accept-microsoft-terms` | the Validation OS image, from Microsoft |
| `winquick capability install` | PowerShell or .NET from Microsoft and GitHub, and the virtio-win ISO for the desktop display driver |
| `winquick cache sync` | your project's NuGet packages, using the host's own `dotnet` |

Nothing else goes out. WinQuick has no analytics, no update check, no crash
reporting and no account: there is no endpoint in the source that is not one of
the downloads above.

The PowerShell and .NET archives are checked against a SHA-256 pinned in the
source before they are unpacked. The Validation OS image is not: Microsoft
revises it in place behind `aka.ms`, so there is no stable hash to pin, and
HTTPS to Microsoft is the only integrity guarantee there is. If that matters to
you, download the image yourself and point `winquick setup --from` at it.

## The boundary

| Direction | What can cross |
|---|---|
| host → guest | the command, the workspace copy, capability volumes, the package-cache copy |
| guest → host | stdout, stderr, the exit code, and files you explicitly request with `--artifact` |

Nothing else. In particular the guest has:

- **no network at all** — no NIC is attached, so no internet, no LAN, no host
- **no access to your filesystem** — only copies of what you named
- **no way to change the Windows image** — the base is opened read-only
- **no persistence between runs** — every run starts from the same frozen state

## What is disposable

Every run gets fresh copies of everything writable:

```
base.qcow2          read-only, never written
  └─ root.qcow2     per-run copy-on-write overlay, deleted afterwards
workspace.img       per-run copy of your project
caps/*.img          per-run copies of PowerShell, .NET, the package cache
artifacts.img       per-run, empty at the start
uefi-vars.fd        per-run, so even firmware state does not carry over
```

Filesystem writes, registry changes and environment variables set by one run are
absent in the next. This is tested, not assumed — see the disposability checks in
`tests/integration.sh`.

## Your source tree is not writable from Windows

`-w <dir>` copies the directory into a disk image. The guest sees a copy at
`C:\workspace` and can write to it freely; none of that reaches the host. A build
script cannot modify, delete or plant files in your project.

The only way out is `--artifact`, which you have to ask for by name.

## Artifact extraction

Artifacts are read out of a filesystem the guest controlled, so their names are
treated as hostile input. WinQuick refuses any entry that is not a single
ordinary path component — anything containing `/`, `\`, a NUL, `.`, `..`, or an
absolute path is skipped with a warning. A guest cannot use a crafted filename to
write outside the artifacts directory.

Artifacts are written to `./winquick-artifacts` by default, never into your
project, and WinQuick refuses to write into a non-empty directory unless you pass
`--artifact-overwrite`.

## The package cache

The canonical NuGet cache lives on your Mac and is written only by host-side
`dotnet restore` when you run `winquick cache sync`. The guest gets a
copy-on-write clone of it, so a build script can write into `%NUGET_PACKAGES%`
all it likes and none of it survives the run or reaches the canonical copy.

This matters: a persistent package cache is exactly the kind of place a hostile
build would try to leave something for a future run to pick up. It cannot.

Verified in the test suite: after a run that writes into the cache, the canonical
image's SHA-256 is unchanged and the next run does not see the file.

## Capability volumes

PowerShell and .NET are downloaded from Microsoft over HTTPS and verified against
pinned SHA-256 digests before use. They are stored as disk images and, like
everything else, cloned per run — a run cannot modify the copy of PowerShell that
later runs get.

## Command construction

Arguments are passed as a Windows command line built with the Windows C-runtime
quoting rules, so an argument containing spaces, quotes or backslashes reaches
the program as one argument rather than being re-split. There is no shell
interpolation of host variables into the guest command.

WinQuick does not interpret the command itself, and does not pass it through a
host shell.

## Results cannot be silently wrong

Every run carries a nonce that the guest echoes back with the exit code. If it
does not match, WinQuick treats the result as invalid, rebuilds, and retries
rather than reporting a confident wrong answer. This exists because a stale read
once produced "exit 0, no output" for a command that never ran — a wrong result
that looks right is worse than a crash.

## Where WinQuick writes on your Mac

Everything is under `~/.winquick`:

```
~/.winquick/
  images/         the Windows runtime built from Microsoft's image
  capabilities/   PowerShell, .NET, the package-cache volume
  caches/nuget/   packages restored on this Mac
  states/         the frozen prepared guest
  cache/          downloaded installers
  run/            transient per-run state, deleted when a run ends
```

`winquick clean` removes generated data; `--all` removes everything. Neither
touches your projects. Files are created with your user's normal permissions;
WinQuick never needs root and never asks for it.

## Desktop automation

The desktop capability gives the guest synthetic keyboard and mouse input and
the ability to read any window's pixels and control tree. That is the feature,
and it is worth being explicit about what it does and does not change.

* **The boundary is unchanged.** Same QEMU isolation as `winquick run`: no
  network in the guest, a disposable disk, and no host filesystem access beyond
  the volumes WinQuick attaches. Input injection and screen capture happen
  *inside* that boundary and do not widen it.
* **This is not a malware-analysis sandbox.** It was not one before the desktop
  capability and it is not one now. WinQuick does not attempt to resist a guest
  that is actively trying to escape.
* **No ports are opened.** The session's QMP socket is a unix socket under
  `~/.winquick/desktop/`. There is no VNC or RDP server and nothing listens on
  TCP.
* **Screenshots are of the guest's screen**, written where you asked on your
  Mac. Nothing is uploaded anywhere.
* **A session is disposable like a run.** It works on a copy-on-write overlay
  over the desktop image; `winquick desktop stop` deletes it, and `winquick
  clean` stops a running one first. Capability volumes are cloned, so a session
  cannot modify what is installed.
* **The desktop image is derived from your Microsoft media** and, like the base
  image, must not be redistributed.

## What is not protected

- **Hypervisor escapes.** WinQuick's isolation is QEMU's and Apple's. A bug in
  either is a bug in WinQuick's boundary. QEMU is a large program with a real
  CVE history; keep it updated.
- **Resource exhaustion.** A run can spin the CPU, fill its disk image and use
  its full memory allocation until the timeout fires. There is no CPU or I/O
  quota.
- **Anything you extract.** Artifacts are files a possibly-hostile guest wrote.
  WinQuick guarantees where they land, not what is in them.
- **Side channels.** No attempt is made to mitigate speculative-execution or
  timing side channels between guest and host.

## Reporting a vulnerability

Open a security advisory on the GitHub repository rather than a public issue.
