# castr Wi-Fi health check verification (2026-09-02)

Windows sender host: this machine, `castr-sender diagnose` / `diagnose --fix`
run from `target/debug/castr-sender.exe` (built from branch `wifi-health-check`
via `cargo build -q -p castr-sender`), unelevated PowerShell/Git-Bash session,
no administrator prompt accepted at any point. Adapter under test: Realtek
8821CE Wireless LAN 802.11ac PCI-E NIC (shared Wi-Fi/Bluetooth combo card),
Wi-Fi disconnected (this host is on Ethernet), Bluetooth active.

## Commands run

```
$ cargo build -q -p castr-sender
$ powercfg /q SCHEME_CURRENT 19cbb8fa-5279-450e-9fac-8a3d5fedd0c1      # before
$ target/debug/castr-sender.exe diagnose
$ target/debug/castr-sender.exe diagnose --fix
$ powercfg /q SCHEME_CURRENT 19cbb8fa-5279-450e-9fac-8a3d5fedd0c1      # after
```

## `castr-sender diagnose` output

```
castr Wi-Fi health check

Adapter: Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC

[ok  ] Wireless display support
       the graphics and Wi-Fi drivers both support wireless display
       Miracast needs both halves; without them Windows will not offer to cast at all.
[ok  ] Driver age
       Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC, version 2024.10.139.3, dated 2024
       Wi-Fi Direct fixes land in vendor drivers; a driver several years old is a common cause of drops.
[warn] Shared Wi-Fi and Bluetooth antenna
       this adapter shares one antenna between Wi-Fi and Bluetooth, and Bluetooth is active
       A Miracast session and Bluetooth traffic then take turns on one antenna, which is a leading cause of mid-cast drops. Turning Bluetooth off during a cast is a reliable test.
[--  ] Station band vs sink band
       the Wi-Fi radio is not connected to a network
       With no station link there is no band to alternate with, which is the best case for casting.
[--  ] Signal strength
       the Wi-Fi radio is not connected to a network
       Signal strength only means something while connected.
[warn] Wireless adapter power saving
       power saving is on (maximum performance on mains, medium power saving on battery)
       Power saving parks the radio between packets, which a Wi-Fi Direct link reads as a dropped peer.
[?   ] Adapter power-off permission
       Get-NetAdapterPowerManagement : A device attached to the system is not functioning.
       When Windows powers the adapter down mid-session the cast ends without explanation.
[--  ] USB selective suspend
       not a USB adapter
       USB selective suspend only affects adapters on the USB bus.

1 of these can be fixed safely. Run `castr-sender diagnose --fix` to be
prompted for each one; every change prints the command that undoes it.

This check cannot change how Windows itself implements Miracast, which lives in
the operating system. It finds the local causes of drops and removes the ones
that are safe to touch. For your own machines, castr's own protocol over the
wire avoids the radio entirely.
```

Process exit code: `1` (the tool exits non-zero when any check is `warn` or
worse, which happened here).

## Comparison against the hand-measured spec values (section 2, recorded 2026-09-02)

| Check | Hand-measured value | Tool output | Result |
|---|---|---|---|
| Adapter identity | Realtek 8821CE | `Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC` | PASS |
| Wireless display support | supported by both halves | `[ok] ... the graphics and Wi-Fi drivers both support wireless display` | PASS |
| Driver age | 2024.10.139.3, dated 2024 | `[ok] ... version 2024.10.139.3, dated 2024` | PASS |
| Shared Wi-Fi/Bluetooth antenna | shared antenna, Bluetooth active | `[warn] ... this adapter shares one antenna between Wi-Fi and Bluetooth, and Bluetooth is active` | PASS |
| Station band vs sink band | Wi-Fi disconnected (host on Ethernet) | `[--] ... the Wi-Fi radio is not connected to a network` | PASS |
| Signal strength | Wi-Fi disconnected, no signal to read | `[--] ... the Wi-Fi radio is not connected to a network` | PASS |
| Wireless adapter power saving | AC index 0 (maximum performance), DC index 2 (medium power saving) | `[warn] power saving is on (maximum performance on mains, medium power saving on battery)` | PASS |
| Adapter power-off permission | `Get-NetAdapterPowerManagement` fails with "A device attached to the system is not functioning" while idle, not elevated | `[?] Get-NetAdapterPowerManagement : A device attached to the system is not functioning.` | PASS |
| USB selective suspend | adapter is not a USB device | `[--] not a USB adapter` | PASS |

9 of 9 rows PASS. Every line matches the hand-measured values from spec
section 2 exactly, including the two warnings (shared antenna, power saving),
the power-off permission reported as Unknown with the same device error text,
and the overall exit code of 1.

## `powercfg` readback before `--fix`

```
Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)
  GUID Alias: SCHEME_BALANCED
  Subgroup GUID: 19cbb8fa-5279-450e-9fac-8a3d5fedd0c1  (Wireless Adapter Settings)
    Power Setting GUID: 12bbebe6-58d6-4636-95bb-3217ef867c1a  (Power Saving Mode)
      Possible Setting Index: 000
      Possible Setting Friendly Name: Maximum Performance
      Possible Setting Index: 001
      Possible Setting Friendly Name: Low Power Saving
      Possible Setting Index: 002
      Possible Setting Friendly Name: Medium Power Saving
      Possible Setting Index: 003
      Possible Setting Friendly Name: Maximum Power Saving
    Current AC Power Setting Index: 0x00000000
    Current DC Power Setting Index: 0x00000002
```

AC index `0` (maximum performance), DC index `2` (medium power saving) — matches
the hand-measured spec values.

## `castr-sender diagnose --fix` output (unelevated shell)

```
castr Wi-Fi health check

Adapter: Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC

[ok  ] Wireless display support
       the graphics and Wi-Fi drivers both support wireless display
       Miracast needs both halves; without them Windows will not offer to cast at all.
[ok  ] Driver age
       Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC, version 2024.10.139.3, dated 2024
       Wi-Fi Direct fixes land in vendor drivers; a driver several years old is a common cause of drops.
[warn] Shared Wi-Fi and Bluetooth antenna
       this adapter shares one antenna between Wi-Fi and Bluetooth, and Bluetooth is active
       A Miracast session and Bluetooth traffic then take turns on one antenna, which is a leading cause of mid-cast drops. Turning Bluetooth off during a cast is a reliable test.
[--  ] Station band vs sink band
       the Wi-Fi radio is not connected to a network
       With no station link there is no band to alternate with, which is the best case for casting.
[--  ] Signal strength
       the Wi-Fi radio is not connected to a network
       Signal strength only means something while connected.
[warn] Wireless adapter power saving
       power saving is on (maximum performance on mains, medium power saving on battery)
       Power saving parks the radio between packets, which a Wi-Fi Direct link reads as a dropped peer.
[?   ] Adapter power-off permission
       Get-NetAdapterPowerManagement : A device attached to the system is not functioning.
       When Windows powers the adapter down mid-session the cast ends without explanation.
[--  ] USB selective suspend
       not a USB adapter
       USB selective suspend only affects adapters on the USB bus.

1 of these can be fixed safely. Run `castr-sender diagnose --fix` to be
prompted for each one; every change prints the command that undoes it.

This check cannot change how Windows itself implements Miracast, which lives in
the operating system. It finds the local causes of drops and removes the ones
that are safe to touch. For your own machines, castr's own protocol over the
wire avoids the radio entirely.

These changes need an administrator prompt, and this window is not elevated.
Re-run `castr-sender diagnose --fix` from an administrator terminal, or run:

  Set wireless adapter power saving to maximum performance
    powercfg /setacvalueindex SCHEME_CURRENT 19cbb8fa-5279-450e-9fac-8a3d5fedd0c1 12bbebe6-58d6-4636-95bb-3217ef867c1a 0
    powercfg /setdcvalueindex SCHEME_CURRENT 19cbb8fa-5279-450e-9fac-8a3d5fedd0c1 12bbebe6-58d6-4636-95bb-3217ef867c1a 0
    powercfg /setactive SCHEME_CURRENT
  to undo:
    powercfg /setacvalueindex SCHEME_CURRENT 19cbb8fa-5279-450e-9fac-8a3d5fedd0c1 12bbebe6-58d6-4636-95bb-3217ef867c1a 0
    powercfg /setdcvalueindex SCHEME_CURRENT 19cbb8fa-5279-450e-9fac-8a3d5fedd0c1 12bbebe6-58d6-4636-95bb-3217ef867c1a 2
    powercfg /setactive SCHEME_CURRENT
```

Exit code: `1`. As expected for an unelevated shell, the one fixable finding
(wireless adapter power saving) needs an administrator prompt; the tool never
attempts it, prints the exact apply and undo `powercfg` commands, and does not
change anything. The other findings have no available fix (`[ok]`, `[--]`) or
are informational only (`[?]`), so nothing else is offered.

## `powercfg` readback after `--fix`

```
Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)
  GUID Alias: SCHEME_BALANCED
  Subgroup GUID: 19cbb8fa-5279-450e-9fac-8a3d5fedd0c1  (Wireless Adapter Settings)
    Power Setting GUID: 12bbebe6-58d6-4636-95bb-3217ef867c1a  (Power Saving Mode)
      Possible Setting Index: 000
      Possible Setting Friendly Name: Maximum Performance
      Possible Setting Index: 001
      Possible Setting Friendly Name: Low Power Saving
      Possible Setting Index: 002
      Possible Setting Friendly Name: Medium Power Saving
      Possible Setting Index: 003
      Possible Setting Friendly Name: Maximum Power Saving
    Current AC Power Setting Index: 0x00000000
    Current DC Power Setting Index: 0x00000002
```

Unchanged: AC `0x00000000`, DC `0x00000002`, identical to the "before"
readback. The unelevated `--fix` run left the powercfg indices exactly where
they were, as expected.

## GUI check

`cargo run -q -p castr-sender` opened the window with a new "Check my Wi-Fi"
button in the top row, beside "Scan". Clicking it (driven via UI Automation
for this verification) ran the check on a worker thread — the window stayed
responsive — and after a moment a "Wi-Fi health" panel appeared below the
receiver list with a scrollable, monospaced report identical in content to
the `diagnose` output above, naming the real `Realtek 8821CE Wireless LAN
802.11ac PCI-E NIC` adapter. The window was then closed.

## Summary

Every check produced by `castr-sender diagnose` on this machine matches the
values recorded by hand in the spec on 2026-09-02: 9 of 9 comparison rows
PASS. The `diagnose --fix` run from an unelevated shell correctly refused to
touch the one fixable setting (wireless power saving), printing the exact
`powercfg` apply and undo commands instead; the `powercfg` readback before and
after `--fix` is byte-for-byte identical (AC `0x00000000`, DC `0x00000002`),
proving nothing was changed.
