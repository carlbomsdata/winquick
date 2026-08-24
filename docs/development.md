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
```

The integration suite drives the real CLI. Some groups skip themselves when the
capability they need is not installed, so a full run wants `powershell`,
`dotnet-sdk` and a synced cache.

## Changing the guest agent

`guest/agent.cmd` is baked into the runtime image, so changing it needs
`winquick setup --force`, not just a prepared-guest rebuild. WinQuick detects the
mismatch and says so — that check exists because the failure mode was otherwise a
mysterious hang.

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
