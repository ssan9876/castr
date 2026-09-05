# castr sub-project 10: one window, both protocols — design (2026-09-04)

Part 3b of the Miracast source project, and the last part that can be built
without a television. Parts 1 (media path), 2 (radio), mode negotiation and 3a
(stop and status) are merged. Part 4, interop against real displays, remains.

## 1. Goal

The sender's window should list Miracast displays beside castr receivers and
cast to either, with PIN entry, monitor selection, and failures shown rather
than printed to a console nobody is looking at.

Success: a person opens `castr-sender`, sees everywhere their screen can go,
picks one, and casts — without knowing which protocol carries it.

## 2. One list, not two

A person picks *where their screen should go*, not which protocol carries it.
So receivers and displays share one list, each row tagged with its kind and the
one fact worth knowing: a receiver's address, a display's bandwidth and whether
it wants content protection.

The two kinds are not identical, and the difference is real rather than
cosmetic: a castr receiver pairs as an explicit step and then casts, while a
Miracast display pairs *inside* the connect, prompting only if Windows has not
paired with it before. So which buttons apply is a function of the selected
row — `actions_for` — rather than a fixed pair of buttons that sometimes lie.

The same machine can legitimately appear twice: the Pi is both a castr receiver
and a Miracast sink. That is not a bug to deduplicate. They are two different
routes to the same screen with different properties, and the list says so.

## 3. Structure

`gui.rs` was 395 dense lines doing four jobs, and this work would have doubled
it. It becomes a directory, split so the decisions land in pure code:

| Unit | Pure | Responsibility |
|---|---|---|
| `gui/targets.rs` | yes | Merge, order, label, and say which actions apply |
| `gui/pin.rs` | yes | PIN validation — six digits for castr, eight for Miracast |
| `gui/session.rs` | no | The active cast, either kind, behind stop/status |
| `gui/wifi.rs` | no | The Wi-Fi report panel, moved unchanged |
| `gui/mod.rs` | no | `App` and the update loop |

`targets.rs` deliberately holds no Windows type. A display arrives as a
portable `DisplayInfo` rather than as the radio crate's `Candidate`, so the
module and its tests build wherever `castr-sender` does.

**Selection is held by identity, not by row number.** A rescan reorders the
list, and an index would then quietly point at a different machine than the one
the user chose. `TargetId` is a receiver's certificate fingerprint or a
display's radio id — both stable, and both distinguishing two machines that
share a name, which two Pis named after their hostname would.

## 4. A second cast lifecycle

castr's cast is an async task driven by tokio channels; a Miracast cast is a
blocking loop that needs its own thread. One `Session` enum covers both, and
the window only ever asks it to *stop* or *describe itself*.

The Miracast half reuses the control channel built in part 3a: the same
`Command::Stop`, the same published snapshot. Two things fall out for free — a
cast started from the window is visible to `miracast-status` and stoppable by
`miracast-stop`, and the one-cast-at-a-time rule covers window and terminal
together rather than each on its own.

Nothing in `miracast_cast.rs` changes.

## 5. The PIN

The existing field was hardcoded to six digits. A Miracast display shows
**eight** — the Pi's own log says `PIN 82128616` — so a Miracast PIN could
never have been submitted through it. `PinKind` carries the length, and the
prompt says how many digits to expect.

The box is armed only when the radio actually asks, so a display Windows
already knows connects with no PIN box at all.

## 6. Status

Each protocol reports what it actually knows. A castr cast shows round trip
time and loss, because the receiver sends them. A Miracast cast shows
throughput sent, repeated frames and how long ago the display answered a
keep-alive — and **no rtt or loss field at all**, because Wi-Fi Display gives a
source no receiver statistics and two permanently blank fields would read as a
perfect link rather than an unmeasured one.

## 7. Monitor selection

New: `castr-capture-win` gains output enumeration — index, device name,
resolution, whether it holds the desktop origin, and whether Windows has it
rotated. Until now `DesktopCapture::new` took a bare index and nothing could
say what any index meant, so choosing a monitor meant a test cast.

The picker appears only when there is more than one monitor, and is locked
while a cast runs because the choice is fixed at start.

A rotated monitor is **labelled as such**, warning that the cast will appear
sideways. The capture path still ignores rotation — that bug is not fixed here,
because it belongs to the capture path rather than to a picker — but the
picker refuses to be silent about it.

## 8. Closing the window

`on_exit` stops a Miracast cast **and waits for the worker to finish**, within
a bound. Closing the window is the likeliest way to leave a display believing a
session is live, which is the same defect class as the Ctrl-C fixed in part 3a.
It joins rather than dropping the worker and hoping.

## 9. Discovery is slow, and must say so

Radio discovery takes about **50 seconds** against four devices in range —
measured, not assumed. Roughly 10 s is the enumeration; the rest is reading
each device's information element one device at a time. It costs the same on
any thread.

So the two scans are independent — the receiver list must not wait a minute for
the radio — and while the radio scan runs the window says what it is waiting
for. A silent minute is indistinguishable from a hang, and was mistaken for one
during development.

Advice about an empty list is withheld until both scans have finished, since
"no Miracast displays" shown while still looking is worse than saying nothing.

## 10. Failures

Every failure reaches the window rather than a console. Both scans report their
errors: a scan that fails silently is indistinguishable from a world with no
displays in it, and this project has already lost days to exactly that.

## 11. Testing, stated plainly

`targets.rs`, `pin.rs` and the output labelling get real tests, and they hold
the decisions worth testing.

**The window itself cannot be verified without a person clicking it.** What can
be shown without one: that it builds, that the pure logic is right, that the
window opens, renders the real merged list, and closes cleanly. What cannot:
that a click on Cast starts a cast, that the PIN box accepts a PIN, that Stop
stops, that the monitor picker changes anything. Those are recorded as NOT RUN
rather than claimed.

## 12. Out of scope

The rotation bug in the capture path. Interop with real displays (part 4).
Changing mode mid-cast for Miracast, which the protocol does not offer. Casting
to more than one target at once.
