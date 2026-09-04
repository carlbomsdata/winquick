# Security policy

## Supported versions

WinQuick is under heavy development and only the latest release is supported.
Fixes go into the next release rather than into patches of older ones.

## Reporting a vulnerability

Open a [security advisory](https://github.com/carlbomsdata/winquick/security/advisories/new)
rather than a public issue. Please include what you ran, what happened, and the
host and QEMU versions — `winquick doctor` prints both.

You will get an acknowledgement within a week. There is no bounty programme.

## What is and is not in scope

WinQuick runs your code inside a real Windows guest under a hardware
hypervisor, which is a strong isolation boundary. **It is not a hardened malware
sandbox and has not been audited as one**, and it is not built for detonating
hostile samples.

[docs/security.md](docs/security.md) states the boundary precisely: what can
cross it in each direction, and what is explicitly not protected — hypervisor
escapes, resource exhaustion, the contents of extracted artifacts, and side
channels. A report that depends on something listed there as unprotected is
already documented rather than a vulnerability.
