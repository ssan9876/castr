# The Windows installer — verification (2026-09-05)

`dist\castr-0.1.0-x64.msi`, built by `scripts\windows\build-msi.ps1` with WiX
5.0.2, installed and uninstalled on `DESKTOP-C6QHH2A` from an elevated
PowerShell by `scripts\windows\verify-msi.ps1`.

## Results

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | The package builds | PASS | 4.7 MB from an 11.4 MB exe |
| 2 | It installs silently | PASS | `msiexec /i /qn` exit 0 |
| 3 | The exe lands in Program Files | PASS | `C:\Program Files\castr\castr-sender.exe` |
| 4 | The installed exe runs | PASS | `--help` lists `miracast-cast` |
| 5 | Start Menu shortcut created | PASS | checked on disk |
| 6 | Desktop shortcut created | PASS | checked on disk |
| 7 | PATH gains the install directory | PASS | log: `UpdateEnvironmentStrings(Name=PATH, Value=C:\Program Files\castr\)` |
| 8 | The firewall rule is added | PASS | `Get-NetFirewallRule -DisplayName 'castr sender'` |
| 9 | Apps and Features lists it | PASS | with version 0.1.0 and publisher `castr` |
| 10 | It offers an uninstall command | PASS | `UninstallString` present |
| 11 | The package declares an icon | PASS | `ARPPRODUCTICON = castr.ico`, `Icon` table holds `castr.ico` |
| 12 | It uninstalls silently | PASS | `msiexec /x /qn` exit 0 |
| 13 | The exe is gone | PASS | |
| 14 | Both shortcuts are gone | PASS | |
| 15 | **PATH is left as it was found** | PASS | the entry is withdrawn, not stranded |
| 16 | **The firewall rule is withdrawn** | PASS | not left authorising a missing executable |
| 17 | Apps and Features no longer lists it | PASS | |
| 18 | `%APPDATA%\castr` survives uninstall | PASS | intended: it holds the identity and every pairing |
| 19 | The icon is embedded in the exe | PASS | `ExtractAssociatedIcon` returns 32x32; version block reads `castr sender` |
| 20 | The icon is legible at every size | PASS | rendered 16-256 on light and dark grounds and looked at |
| 21 | The install wizard looks right | **NOT RUN** | needs a person to look at it |
| 22 | Apps and Features draws the icon | **NOT RUN** | same |
| 23 | Uninstalling from Apps and Features works | **NOT RUN** | only the silent `/x` path was exercised |
| 24 | Upgrade over an older version | NOT RUN | there is no older version yet |
| 25 | Group-policy deployment | NOT RUN | the reason MSI was chosen, untested |

Rows 15 and 16 are the ones worth the most: the PATH entry and the firewall
rule are what installers most often leave behind, and both are declared inside
components so they are withdrawn with them rather than by a script that might
not run.

## Verified without installing

Before asking anyone for administrator rights, the package was taken apart:

- `msiexec /a` extracted `PFiles64\castr\castr-sender.exe`, confirming the
  layout.
- Its database was read directly: five features all at level 1, two shortcuts
  both referencing `castr.ico`, the `Environment` row as `=-*PATH` with
  `[~];[INSTALLFOLDER]` (append, remove on uninstall), the `Wix5FirewallException`
  row scoped to `LocalSubnet`, and two `Upgrade` rows so a major upgrade is
  detected in both directions.
- Six firewall custom actions are scheduled, **including the uninstall pair** —
  which is what makes row 16 something other than hope.

## Two failures that were the test's, not the package's

The first run reported 18 of 20. Both failures were in how the checking was
done, and the install log settled both.

**PATH.** The check compared the machine PATH against `Split-Path $exe`, which
has no trailing separator. MSI resolves `[INSTALLFOLDER]` *with* one, so the
entry reads `C:\Program Files\castr\` and the comparison silently missed. The
log said plainly what had been written. Windows does not care about the
trailing backslash; the check now normalises both sides.

**The icon.** The check read `DisplayIcon` under the Uninstall registry key,
which is where an old-style Win32 installer puts it. An MSI registers its icon
on the product instead. A second attempt read `ProductIcon` through the
installer API and also came back empty.

At that point the right move was to stop: the log showed
`IconCreate(Icon=castr.ico)` and `ProductIcon=castr.ico`, so the package was
demonstrably doing its part, and what remained in doubt was whether *Windows
draws it* — a question about Windows, and a visual one. The check now asserts
what the package declares, which is knowable, and rows 22 is marked NOT RUN
like the wizard, which is honest.

The pattern is worth naming, because it is the third time today: **a failing
check is a claim about the checker until the evidence says otherwise.** Twice
the code was right and the test was wrong; changing the package to satisfy
either check would have made it worse.

## Decisions worth recording

**WiX 5, not 7.** WiX 7 refuses to run until you accept the Open Source
Maintenance Fee licence. That is a commitment with possible commercial
implications and not one to accept on someone's behalf, so the toolchain is
pinned to 5.0.2, which is supported and carries no such condition. The
extensions must be pinned to match: an unpinned `wix extension add` installs
7.0.0 against a 5.x toolset and fails with a confusing "could not find expected
package root folder wixext5".

**The trailing backslash in PATH is left alone.** Stripping it needs a custom
action; Windows handles it; every MSI that adds to PATH does the same.

**Unsigned.** Windows will call the publisher unknown and SmartScreen will
warn. Signing needs a certificate, which is a purchase rather than a change.

## Not yet answered

- Everything in rows 21 to 25.
- Whether the four optional features can actually be declined in the wizard.
  They are authored as features at level 1 and the feature tree is the standard
  `WixUI_FeatureTree`, but nobody has clicked one off and confirmed the result.
- What an upgrade does to the PATH entry and firewall rule, which is where
  upgrade bugs usually live.
