# One window, both protocols — verification (2026-09-04)

Hardware: Windows PC `DESKTOP-C6QHH2A`, three monitors, with the Pi 3 B sink,
a Samsung 75" Crystal UHD, an Amazon Fire TV Stick and an Epson printer in
range.

**A graphical interface cannot be verified without a person clicking it.** This
document is deliberately split into what was shown and what was not, and the
second list is longer than anyone would like.

## What was shown

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | The pure decisions are correct | PASS | 121 tests in `castr-sender` (39 new) and 23 in `castr-capture-win` (7 new); `cargo clippy --workspace --tests` adds no new warning |
| 2 | Monitors enumerate on real hardware | PASS | `DISPLAY1 3440x1440 (primary)`, `DISPLAY2 1080x1920 (rotated…)`, `DISPLAY3 1920x1080` |
| 3 | A rotated monitor is identified as one | PASS | DISPLAY2 above. Not hypothetical — this machine has one |
| 4 | The window opens and renders | PASS | Screenshot, `2026-09-04-gui-one-list.png` |
| 5 | Receivers and displays share one list, correctly labelled | PASS | `DietPi  castr · 192.168.88.157`, `75" Crystal UHD  Miracast · up to 54 Mbps · HDCP`, `DietPi  Miracast · up to 10 Mbps` |
| 6 | A display wanting content protection is flagged before casting | PASS | The Samsung's row, above |
| 7 | Ordering is receivers first, then displays by name | PASS | Same screenshot |
| 8 | The same machine may appear under both protocols | PASS | The Pi appears twice, as intended |
| 9 | The monitor picker shows the real primary | PASS | `DISPLAY1 3440x1440 (primary)` in the window |
| 10 | Pair and Cast are disabled with nothing selected | PASS | Both greyed in the screenshot |
| 11 | The slow radio scan says what it is waiting for | PASS | `Looking for Miracast displays - this takes about a minute`, with a spinner, cleared once found |
| 12 | Advice is withheld until both scans finish | PASS | The advice line is absent mid-scan and absent afterwards, the list being non-empty |
| 13 | The window closes cleanly | PASS | `CloseMainWindow`, exit code 0 |
| 14 | Discovery works off the main thread | PASS | `discovery_finishes_on_a_worker_thread_though_slowly`, 50 s |

## What was not

Every row here needs a hand on a mouse. None is claimed.

| # | Claim | Verdict |
|---|---|---|
| 15 | Clicking a row selects it | NOT RUN |
| 16 | Cast to a castr receiver works from the window | NOT RUN |
| 17 | Cast to a Miracast display works from the window | NOT RUN |
| 18 | The PIN box accepts an eight-digit Miracast PIN and pairs | NOT RUN |
| 19 | Stop stops a cast started from the window | NOT RUN |
| 20 | Choosing a different monitor casts that monitor | NOT RUN |
| 21 | Closing the window mid-cast tears down before exiting | NOT RUN |
| 22 | A second cast is refused while one runs | NOT RUN |
| 23 | A failure appears in the window rather than nowhere | NOT RUN |

Row 21 is the one worth the most attention. It is the same defect class as the
Ctrl-C fixed earlier today — a display left believing a session is live — and
the code path that prevents it has never been executed.

## The screenshot

`2026-09-04-gui-one-list.png`, taken with `PrintWindow`, which renders a window
regardless of its z-order. Worth recording as a technique: the obvious approach
of raising the window and photographing the screen produced a picture of the
wallpaper, because Windows will not let a background process steal the
foreground, and nothing about the result said so.

## A wrong hypothesis, implemented and reverted

The radio scan appeared to hang: the window's spinner was still turning after
20 seconds with no result and no error.

The hypothesis was that WinRT's `.get()` on a worker thread with no
multi-threaded apartment waits for a message pump the thread does not have. It
is a real failure mode, it fitted the symptom, and a `CoInitializeEx` was
written, with a confident doc comment, and shipped into the radio crate.

It was wrong. The test still failed at a 30-second bound, and a step-by-step
probe then completed in ten seconds — the difference being that the probe
skipped the per-device information element read. Timing both paths settled it:

| Path | Thread | Time |
|---|---|---|
| `radio::discover()` | worker | ~50 s |
| `castr-sender miracast-list` | main | ~50 s |

Identical. Discovery was never hanging and threading was never involved; it is
simply slow, and nobody had ever timed it because the command line prints its
answer and exits. The `CoInitializeEx` and its feature flag were reverted
rather than kept as harmless, because it carried an explanation that was not
true, and unverified explanations in this codebase are what the verification
documents exist to prevent.

What survives is what was actually learned: a measured figure in `discover`'s
documentation, a test that asserts discovery finishes off the main thread with
a bound generous enough not to lie about speed, and a line in the window saying
the wait is expected.

Two lessons, both already seen today in a different costume:

- **A plausible mechanism that fits the symptom is not evidence.** The apartment
  story explained everything and was still false. What settled it was timing
  both paths, which took two minutes and could have come first.
- **Slow is not stuck, and the difference is invisible without a number.** The
  fix the evidence actually supported was a label, not a code change.

## Not yet answered

- Everything in the second table.
- Whether the Fire TV appears when its mirroring page is open. It advertised
  nothing during this session, as it does when idle.
- Whether 50 seconds can be reduced. The per-device element read dominates it
  and is done one device at a time; whether it parallelises is unexamined.
