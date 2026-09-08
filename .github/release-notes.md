Two Windows installers for the castr sender. Both hold the same exe.

| | Installs to | Elevation | Firewall rule |
|---|---|---|---|
| `castr-<version>-x64.msi` | `C:\Program Files\castr` | asks once | included |
| `castr-<version>-x64-peruser.msi` | `%LOCALAPPDATA%\Programs\castr` | none at all | see below |

Take the per-machine one if you have administrator rights on the machine, or
if you are deploying by group policy. Take the per-user one if you do not, or
would rather not be asked.

Either way the wizard offers a Start Menu shortcut, a Desktop shortcut, and
adding `castr-sender` to your PATH — decline any of them.

### The firewall rule

The per-user package cannot add one: firewall rules live in the machine-wide
policy store, so there is no such thing as a per-user rule and no installer can
create one without elevation. Once, from an administrator terminal:

```
castr-sender firewall --allow
```

Without it, a Miracast display cannot connect back to this machine on port
7236, and casting to a real display adapter times out — the adapter dials you,
it does not listen. Casting to a castr receiver is unaffected; that connection
is dialled out from here. `castr-sender firewall` on its own says whether the
rule is there and prints the command that adds it.

The installer is optional either way: `castr-sender.exe` is a single portable
exe with no runtime DLLs, and runs from wherever you put it. `firewall --allow`
covers whichever copy is running, so the portable exe can have the rule too.

### Signing

**These MSIs are unsigned**, so Windows will call the publisher unknown and
SmartScreen will warn about them. Signing needs a code-signing certificate,
which is a purchase rather than a code change.

### Uninstalling

Through Apps & Features. It removes the PATH entry, the shortcuts, and (for the
per-machine package) the firewall rule, but deliberately keeps
`%APPDATA%\castr\` — the identity certificate and the pairings, which a
reinstall would otherwise have to redo. A rule added by `firewall --allow` is
yours to remove with `castr-sender firewall --remove` before you uninstall.
