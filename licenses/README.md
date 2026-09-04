# Licence texts shipped with WinQuick

WinQuick distributes binaries built from `ntfsprogs` (GPL-2.0-or-later) and, on
Windows, `hivex` (LGPL-2.1-or-later). Those licences require their text to
travel with the binaries, so the release archives carry a copy.

The copies live here rather than being downloaded when a release is built.
Fetching them made the build depend on `gnu.org` being reachable at exactly the
wrong moment, which it repeatedly was not — the release stopped on a connection
timeout, twice on GitHub's runners and once on the Windows machine. A licence
we are obliged to ship is not something to hope to download.

| File | Source |
|---|---|
| `GPL-2.0.txt` | <https://www.gnu.org/licenses/old-licenses/gpl-2.0.txt> |
| `LGPL-2.1.txt` | <https://www.gnu.org/licenses/old-licenses/lgpl-2.1.txt> |

    edaef632cbb643e4e7a221717a6c441a4c1a7c918e6e4d56debc3d8739b233f6  GPL-2.0.txt
    20e50fe7aae3e56378ebf0417d9de904f55a0e61e4df315333e632a4d3555d95  LGPL-2.1.txt

These are the Free Software Foundation's texts, unmodified. See
[../docs/licensing.md](../docs/licensing.md) for what WinQuick may and may not
redistribute.
