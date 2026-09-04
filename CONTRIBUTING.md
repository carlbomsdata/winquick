# Contributing

Bug reports are the most useful thing you can send. Include the command you
ran, what you expected, what happened, and the output of `winquick doctor`.
Adding `--verbose` to a failing command usually says why it failed.

## Before opening a pull request

Please open an issue first for anything beyond a fix. WinQuick has deliberate
scope limits, and several of them are settled decisions with measurements
behind them — [docs/research.md](docs/research.md) has the evidence, and
[docs/architecture.md](docs/architecture.md) the shape it produced.

## What the checks require

```console
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs those on macOS, Linux and Windows, and also builds the release
archives. The unit suite is pure host logic. Anything that needs a real Windows
guest lives in `tests/integration.sh`, which needs a working installation and
runs on real hardware rather than in CI.

## House rules

- Measure rather than assert. A performance claim belongs in
  `docs/research.md` with the numbers that produced it.
- Do not commit Microsoft binaries or images, in any form.
- Preserve stdout, stderr and exit codes exactly.

By contributing you agree that your work is licensed under Apache-2.0.
