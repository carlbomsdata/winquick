# Development

## Building

```console
cargo build --release
./scripts/build-ntfs-helpers.sh     # once; produces vendor/ntfsprogs
cargo test --release
```

The helpers are found automatically from a source checkout (`vendor/ntfsprogs`),
so `cargo run -- setup` works without installing anything.

## Layout

| | |
|---|---|
| `src/main.rs` | CLI surface, argument quoting |
| `src/setup.rs` | building a runtime from Microsoft's image |
| `src/runner.rs` | the two execution paths and the fallback chain |
| `src/state.rs` | prepared-guest lifecycle and fingerprinting |
| `src/capability.rs` | optional volumes and the package cache |
| `src/mailbox.rs` | the host↔guest protocol |
| `src/artifact.rs` | getting files back out |
| `src/qemu.rs`, `src/qmp.rs` | everything that knows QEMU exists |
| `src/lock.rs`, `src/interrupt.rs` | concurrency and Ctrl-C |
| `guest/agent.cmd` | the ~40 lines that run inside Windows |

## Tests

```console
cargo test --release             # unit tests
./tests/integration.sh 30        # against a real runtime; last arg = warm-run count
./tests/firstrun.sh <image>      # install from nothing, in a throwaway HOME
```

The integration suite drives the real CLI. Some groups skip themselves when the
capability they need is not installed, so a full run wants `powershell`,
`dotnet-sdk` and a synced cache.

`firstrun.sh` answers a different question, and it is the one a new user asks:
it overrides `HOME`, installs a runtime from a Validation OS image you pass on
the command line, and runs what the readme tells a new user to run. Everything
else here starts from a working `~/.winquick`, which is how a defect in `setup`
or in the freshly built agent can survive a green suite. Run it before releasing.

## Changing the guest agent

`guest/agent.cmd` is baked into the runtime image, so changing it needs
`winquick setup --force`, not just a prepared-guest rebuild. WinQuick detects the
mismatch and says so — that check exists because the failure mode was otherwise a
mysterious hang.

**And `setup --force` is not enough on its own.** The serviced images carry their
own copy of the agent and their own copy of the metadata, so each has to be
rebuilt by the capability that built it:

```console
winquick setup --force
winquick capability install dotnet-framework --force   # if installed
winquick capability install desktop --force            # if installed
```

`winquick doctor` names whichever of these is stale, and the naming matters:
`run` boots the .NET Framework image when it is present, so an agent change
leaves `winquick run` failing until that image is rebuilt, and `setup --force`
alone rebuilds the one image that was already fine.

Because the agent is what the host waits on, a change here is also the kind that
a green `integration.sh` can miss. Run `firstrun.sh` too: it builds the image and
the agent from scratch and then asks whether Windows answers.

## Things that look wrong but are not

Collected because each cost real time. The reasoning is in
[research.md](research.md).

- **The UEFI variable store must stay writable.** Read-only makes Windows fail to
  boot at all, silently, with a black framebuffer.
- **Volumes must be attached writable.** Windows writes when mounting; a
  read-only NVMe makes those fail with `aio failed: Operation not permitted` and
  no volume appears.
- **Never reformat the mailbox, workspace or capability images between runs.**
  The guest re-reads them via a volume GUID derived from the filesystem;
  reformatting changes it and the guest can never mount them again. Clone and
  rewrite the contents instead.
- **Do not use `savevm`/`loadvm`.** It requires every writable block device to
  support snapshots, and picks the wrong device for the VM state. Migration to a
  file is the working design.
- **Do not add more `mountvol` remount cycles.** One (the workspace) is reliable;
  three destabilised the mailbox mount and produced silent stale reads.

## Releasing

```console
./scripts/release.sh 0.1.0
```

Builds, packages, checksums and writes `dist/`. See the script for the signing
and notarization steps, which need Apple Developer credentials.
