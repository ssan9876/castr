The Windows installer for the castr sender. Download the `.msi` below and
double-click it.

The wizard asks for a licence and an install location, then offers four things
you can decline: a Start Menu shortcut, a Desktop shortcut, adding
`castr-sender` to the PATH, and a Windows Firewall rule. The firewall rule is
worth keeping — castr listens on 7236 for a Miracast display to connect back,
and a firewall prompt arriving mid-cast is the worst possible moment for one.

The installer is optional: `castr-sender.exe` is a single portable exe with no
runtime DLLs, and runs from wherever you put it.

**The MSI is unsigned**, so Windows will call the publisher unknown and
SmartScreen will warn about it. Signing needs a code-signing certificate, which
is a purchase rather than a code change.

Uninstalling through Apps & Features removes the PATH entry and the firewall
rule as well as the files, but deliberately keeps `%APPDATA%\castr\` — the
identity certificate and the pairings, which a reinstall would otherwise have
to redo.
