# The timer probe

Two tools from the synthetic-timer investigation.

`timerprobe.py` emits a base64 PowerShell command that runs one WinQuick
command on a Windows host and classifies it **from what WinQuick says about
itself**, never from elapsed time:

```
python3 timerprobe.py <cpus> <label> "<command>" <timeout> <cold:yes|no> <allowcold:yes|no>
ssh tobias@ROAD-WARRIOR01 "powershell -NoProfile -EncodedCommand $(...)"
```

It refuses to run when `~/.winquick/restore-unsupported` is present, and exits
non-zero when a run that was supposed to be warm came back cold. Both guards
exist because they were learned the hard way: that note is keyed on the QEMU
binary's identity, so rebuilding QEMU creates an identity the `restore-works`
note does not cover, one failed attempt silently disables the fast path, and a
whole round of "fast warm runs" turned out to be cold boots.

`synicdiag.py` patches a lab QEMU to dump, at the freeze and again after the
restore, every piece of Hyper-V timing state public WHP will hand over: the
SynIC registers, `VpRuntime`, `GuestOsId`, the hypercall/VP-assist/reference-TSC
overlays, `TscVirtualOffset`, and the 200-byte `SynicTimerState` blob as hex.
Set `WHPX_SYNIC_DIAG` to a file path to enable it. It is a laboratory
instrument and is deliberately not part of the shipped patch stack.

Do not use `ping -n 1` as a timer workload -- it sends one packet and waits for
nothing. `ping -n 2` is the smallest thing in a stock Validation OS that waits
about a second. `timeout.exe` does not exist there.
