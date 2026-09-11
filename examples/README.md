# Examples

Worked examples for WinQuick. Each is self-contained and uses only the public
CLI.

## WpfDemo

A small WPF application, built and driven end to end inside a headless Windows
desktop session. See [WpfDemo/](WpfDemo/) and [docs/desktop.md](../docs/desktop.md).

```console
winquick capability install dotnet-sdk
winquick capability install desktop
winquick ui-test examples/WpfDemo/WpfDemo.csproj --script examples/WpfDemo/smoke.uitest
```

## Bring your own tools (Go, Node, Python, …)

The guest ships only the .NET capability, but it is a real Windows and runs what
Windows runs. Register any toolchain once as a *tool volume* and every later run
keeps it cached and on `PATH`, with no re-copy — a 300 MiB toolchain does not
turn a one-second command into a two-minute one.

No toolchain binaries are committed here; download the native `win-arm64` build
of whichever tool you want and point `winquick tool add` at it. Go, for example:

```console
# One-off: download the Windows ARM64 Go toolchain and register it.
curl -LO https://go.dev/dl/go1.23.4.windows-arm64.zip
unzip -q go1.23.4.windows-arm64.zip          # -> ./go/bin/go.exe
winquick tool add go --from ./go             # WinQuick puts ./go/bin on PATH

# Every run now has `go` on PATH, cached — no re-copy.
echo 'package main; import "fmt"; func main(){ fmt.Println("hi") }' > hello.go
winquick run -w . -a hello.exe -- cmd /c "go build -o hello.exe hello.go && hello.exe"
# -> ./winquick-artifacts/hello.exe   (a real PE32+ ARM64 executable)

winquick tool list
winquick tool remove go
```

The same shape works for Node (`node-vXX-win-arm64.zip`, `node.exe` at the root),
Python (`python-X-embed-arm64.zip`), or any portable CLI. WinQuick puts a `bin`
subdirectory on `PATH` if the toolchain has one, otherwise the volume root; name
the directories yourself with `--path <dir>` (repeatable).

Measured cold/warm times for Go, Node and Python are in
[docs/research.md](../docs/research.md) under "Bring-your-own toolchains".
