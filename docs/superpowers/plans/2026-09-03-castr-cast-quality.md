# Cast Quality Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the mouse cursor into the cast, and repair a lost delta-frame fragment inside the stall the receiver already spends waiting for it.

**Architecture:** The cursor is composited into the BGRA frame at the sender, on the CPU, by a pure `blend` function that needs no hardware to test. The repair works by deleting a rule at each end: the receiver stops refusing to ask for deltas, and the sender stops refusing to answer, leaving one policy in the only place that knows the playout deadline.

**Tech Stack:** Rust, the `windows` crate's Desktop Duplication API (already a dependency), no new crates.

**Spec:** `docs/superpowers/specs/2026-09-03-castr-cast-quality-design.md`

## Global Constraints

- **No new crates.** Nothing is added to any `Cargo.toml` except feature flags already available on the existing `windows` dependency.
- **`cursor.rs` stays pure.** No `windows` crate types, no D3D, no I/O. It is the only reason the three pixel formats are testable, and the tests run on any machine.
- **The cursor never corrupts a frame.** Every failure draws nothing or draws the cached shape. A missing cursor over a correct desktop is always the right outcome.
- **No latency is added to either mode.** A repair that cannot arrive before the existing `GAP_WAIT_US` window expires is never requested.
- **`DECODE_MARGIN_US` is 10_000.** `GAP_WAIT_US` is 150_000 and already exists in `crates/castr-media/src/jitter.rs`.
- **Keyframe NACKs keep their current unconditional behaviour.** The deadline applies to deltas only — a late keyframe is still worth asking for, because without one nothing decodes at all.
- **Comments explain why, not what.** Match the surrounding code, which comments the reasoning behind a decision and never narrates the code.
- **Verification commands.** Windows: `cargo test -q --workspace`. Linux crates plus clippy with `-D warnings`: `bash scripts/pi/test-linux.sh`. Cross-build: `bash scripts/pi/build-pi.sh`. All three must pass before any commit that touches shared crates.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/castr-capture-win/src/cursor.rs` (create) | Pure: cursor shape data, the three blend formats, clipping. No Windows types. |
| `crates/castr-capture-win/src/lib.rs` (modify) | Declare the module. |
| `crates/castr-capture-win/src/dxgi.rs` (modify) | Ask DXGI for position and shape, cache the shape, composite before returning the frame. |
| `crates/castr-capture-win/Cargo.toml` (modify) | One `windows` feature flag for the pointer-shape types. |
| `crates/castr-media/src/jitter.rs` (modify) | Export `GAP_WAIT_US` so the receiver can compute the repair deadline from it. |
| `crates/castr-proto/src/reassemble.rs` (modify) | NACK incomplete deltas, but only while a repair could still arrive. |
| `crates/castr-receiver/src/pipeline.rs` (modify) | Pass the round-trip time and the repair window into `tick`. |
| `crates/castr-net/src/retransmit.rs` (modify) | Honour any NACK for a frame still held. |
| `crates/castr-net/tests/repair.rs` (create) | End-to-end: a dropped fragment is repaired and the frame completes. |
| `docs/superpowers/verification/2026-09-03-castr-cast-quality-e2e.md` (create) | What the hardware showed, including the lip-sync number. |

---

### Task 1: Cursor shapes and blending

**Files:**
- Create: `crates/castr-capture-win/src/cursor.rs`
- Modify: `crates/castr-capture-win/src/lib.rs`

**Interfaces:**
- Consumes: nothing. Pure module.
- Produces: `cursor::{CursorKind, CursorShape, blend}`. `CursorKind` is `{Color, Monochrome, MaskedColor}`. `CursorShape` has public fields `kind: CursorKind`, `width: u32`, `height: u32`, `pitch: u32`, `hotspot_x: i32`, `hotspot_y: i32`, `data: Vec<u8>`, and a method `drawn_height(&self) -> u32`. `blend(shape: &CursorShape, x: i32, y: i32, dst: &mut [u8], dst_width: u32, dst_height: u32, dst_stride: u32)`.

This is the task with the real test surface. The monochrome format is where bugs hide, so its tests come first and are exhaustive.

- [ ] **Step 1: Write the failing tests**

Create `crates/castr-capture-win/src/cursor.rs` containing only the module doc and this test module:

```rust
//! Cursor shapes, and drawing one into a BGRA frame.
//!
//! Windows excludes the cursor from the duplicated desktop texture and hands
//! it over separately, in three formats — two of which predate alpha
//! channels. This module is deliberately free of Windows types so all three
//! can be tested anywhere: the arrow and the text I-beam are monochrome, and
//! getting their mask rules wrong renders the text cursor as a black
//! rectangle.

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4x4 BGRA canvas filled with one colour.
    fn canvas(b: u8, g: u8, r: u8) -> Vec<u8> {
        let mut v = Vec::new();
        for _ in 0..16 {
            v.extend_from_slice(&[b, g, r, 255]);
        }
        v
    }

    fn px(buf: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
        let i = (y * 16 + x * 4) as usize;
        (buf[i], buf[i + 1], buf[i + 2])
    }

    /// One opaque red pixel, 1x1, no hotspot.
    fn one_red_pixel() -> CursorShape {
        CursorShape {
            kind: CursorKind::Color,
            width: 1,
            height: 1,
            pitch: 4,
            hotspot_x: 0,
            hotspot_y: 0,
            data: vec![0, 0, 255, 255],
        }
    }

    #[test]
    fn a_colour_cursor_alpha_blends_over_the_desktop() {
        let mut dst = canvas(0, 0, 0);
        blend(&one_red_pixel(), 1, 2, &mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 1, 2), (0, 0, 255), "opaque red lands");
        assert_eq!(px(&dst, 0, 0), (0, 0, 0), "nothing else is touched");
    }

    #[test]
    fn a_half_transparent_pixel_mixes_with_what_is_under_it() {
        let mut dst = canvas(0, 0, 0);
        let shape = CursorShape {
            data: vec![0, 0, 255, 128],
            ..one_red_pixel()
        };
        blend(&shape, 0, 0, &mut dst, 4, 4, 16);
        let (b, g, r) = px(&dst, 0, 0);
        assert_eq!((b, g), (0, 0));
        assert!((127..=129).contains(&r), "about half red, got {r}");
    }

    #[test]
    fn a_fully_transparent_pixel_changes_nothing() {
        let mut dst = canvas(10, 20, 30);
        let shape = CursorShape {
            data: vec![0, 0, 255, 0],
            ..one_red_pixel()
        };
        blend(&shape, 0, 0, &mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 0, 0), (10, 20, 30));
    }

    /// An 8x1 monochrome cursor: the AND row then the XOR row, one byte each.
    /// Bit 7 is the leftmost pixel.
    fn mono(and_byte: u8, xor_byte: u8) -> CursorShape {
        CursorShape {
            kind: CursorKind::Monochrome,
            width: 8,
            height: 2,
            pitch: 1,
            hotspot_x: 0,
            hotspot_y: 0,
            data: vec![and_byte, xor_byte],
        }
    }

    #[test]
    fn monochrome_covers_all_four_mask_combinations() {
        // Leftmost four pixels, in order: AND=0/XOR=0, AND=0/XOR=1,
        // AND=1/XOR=0, AND=1/XOR=1.
        // AND bits: 0,0,1,1 -> 0b0011_0000 = 0x30
        // XOR bits: 0,1,0,1 -> 0b0101_0000 = 0x50
        let mut dst = canvas(10, 20, 30);
        blend(&mono(0x30, 0x50), 0, 0, &mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 0, 0), (0, 0, 0), "AND=0 XOR=0 is black");
        assert_eq!(px(&dst, 1, 0), (255, 255, 255), "AND=0 XOR=1 is white");
        assert_eq!(px(&dst, 2, 0), (10, 20, 30), "AND=1 XOR=0 is transparent");
        assert_eq!(
            px(&dst, 3, 0),
            (245, 235, 225),
            "AND=1 XOR=1 inverts what is under it"
        );
    }

    #[test]
    fn a_monochrome_shape_draws_half_its_declared_height() {
        let s = mono(0, 0);
        assert_eq!(s.height, 2, "the buffer holds both masks");
        assert_eq!(s.drawn_height(), 1, "only one row is drawn");
    }

    #[test]
    fn masked_colour_treats_alpha_as_a_switch_not_a_blend() {
        let mut dst = canvas(10, 20, 30);
        let shape = CursorShape {
            kind: CursorKind::MaskedColor,
            width: 2,
            height: 1,
            pitch: 8,
            hotspot_x: 0,
            hotspot_y: 0,
            // First pixel alpha 0x00: copy. Second alpha 0xFF: XOR.
            data: vec![1, 2, 3, 0x00, 0xff, 0xff, 0xff, 0xff],
        };
        blend(&shape, 0, 0, &mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 0, 0), (1, 2, 3), "alpha 0 copies");
        assert_eq!(px(&dst, 1, 0), (245, 235, 225), "alpha 255 inverts");
    }

    #[test]
    fn the_hotspot_shifts_where_the_cursor_lands() {
        // The caller subtracts the hotspot, so blend itself draws at the
        // coordinate it is given; this test pins that contract so the
        // subtraction cannot silently migrate into blend.
        let mut dst = canvas(0, 0, 0);
        let shape = CursorShape {
            hotspot_x: 3,
            hotspot_y: 3,
            ..one_red_pixel()
        };
        blend(&shape, 2, 2, &mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 2, 2), (0, 0, 255), "drawn at the given point");
    }

    #[test]
    fn a_cursor_overhanging_an_edge_is_clipped_not_skipped() {
        for (x, y, vx, vy) in [(-1i32, 0i32, 0u32, 0u32), (3, 0, 3, 0), (0, -1, 0, 0), (0, 3, 0, 3)] {
            let mut dst = canvas(0, 0, 0);
            let shape = CursorShape {
                width: 2,
                height: 2,
                pitch: 8,
                data: vec![0, 0, 255, 255].repeat(4),
                ..one_red_pixel()
            };
            blend(&shape, x, y, &mut dst, 4, 4, 16);
            assert_eq!(
                px(&dst, vx, vy),
                (0, 0, 255),
                "the visible part still draws at ({x},{y})"
            );
        }
    }

    #[test]
    fn a_cursor_entirely_off_screen_draws_nothing_and_does_not_panic() {
        let mut dst = canvas(7, 7, 7);
        blend(&one_red_pixel(), 99, 99, &mut dst, 4, 4, 16);
        blend(&one_red_pixel(), -99, -99, &mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 0, 0), (7, 7, 7));
    }

    #[test]
    fn a_truncated_shape_buffer_is_survived_rather_than_panicking() {
        // A driver that reports a size it does not deliver must not crash the
        // capture thread mid-cast.
        let mut dst = canvas(5, 5, 5);
        let shape = CursorShape {
            width: 4,
            height: 4,
            pitch: 16,
            data: vec![0, 0, 255, 255],
            ..one_red_pixel()
        };
        blend(&shape, 0, 0, &mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 0, 0), (0, 0, 255), "what was delivered is drawn");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -q -p castr-capture-win cursor::`
Expected: FAIL to compile — `CursorShape`, `CursorKind` and `blend` do not exist.

- [ ] **Step 3: Write the implementation**

Add above the test module in `crates/castr-capture-win/src/cursor.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    /// 32-bit BGRA with a real alpha channel.
    Color,
    /// 1 bit per pixel: an AND mask with the XOR mask stacked underneath it,
    /// in one buffer of double height. The standard arrow and the text
    /// I-beam are still this.
    Monochrome,
    /// 32-bit BGRA where alpha is a switch rather than a blend.
    MaskedColor,
}

#[derive(Debug, Clone)]
pub struct CursorShape {
    pub kind: CursorKind,
    pub width: u32,
    /// Rows in `data`. For `Monochrome` this is twice the drawn height.
    pub height: u32,
    /// Bytes per row in `data`.
    pub pitch: u32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    pub data: Vec<u8>,
}

impl CursorShape {
    /// Rows actually drawn, which is half the buffer for a monochrome shape.
    pub fn drawn_height(&self) -> u32 {
        match self.kind {
            CursorKind::Monochrome => self.height / 2,
            _ => self.height,
        }
    }
}

/// Draws `shape` into a BGRA buffer with its top-left corner at `(x, y)`.
///
/// `x` and `y` are already hotspot-adjusted by the caller. Anything falling
/// outside the destination is clipped, and a `data` buffer shorter than the
/// declared dimensions draws what it has: a cursor that is wrong is far
/// better than a capture thread that panics mid-cast.
pub fn blend(
    shape: &CursorShape,
    x: i32,
    y: i32,
    dst: &mut [u8],
    dst_width: u32,
    dst_height: u32,
    dst_stride: u32,
) {
    let rows = shape.drawn_height();
    for cy in 0..rows {
        let dy = y + cy as i32;
        if dy < 0 || dy as u32 >= dst_height {
            continue;
        }
        for cx in 0..shape.width {
            let dx = x + cx as i32;
            if dx < 0 || dx as u32 >= dst_width {
                continue;
            }
            let di = (dy as u32 * dst_stride + dx as u32 * 4) as usize;
            if di + 3 >= dst.len() {
                continue;
            }
            match shape.kind {
                CursorKind::Color => {
                    let si = (cy * shape.pitch + cx * 4) as usize;
                    let Some(src) = shape.data.get(si..si + 4) else {
                        continue;
                    };
                    let a = src[3] as u32;
                    if a == 0 {
                        continue;
                    }
                    for c in 0..3 {
                        let s = src[c] as u32;
                        let d = dst[di + c] as u32;
                        dst[di + c] = ((s * a + d * (255 - a)) / 255) as u8;
                    }
                }
                CursorKind::Monochrome => {
                    let byte = (cx / 8) as usize;
                    let bit = 7 - (cx % 8) as u8;
                    let and_i = (cy * shape.pitch) as usize + byte;
                    let xor_i = ((cy + rows) * shape.pitch) as usize + byte;
                    let (Some(&a_byte), Some(&x_byte)) =
                        (shape.data.get(and_i), shape.data.get(xor_i))
                    else {
                        continue;
                    };
                    let and = (a_byte >> bit) & 1;
                    let xor = (x_byte >> bit) & 1;
                    match (and, xor) {
                        (0, 0) => dst[di..di + 3].fill(0),
                        (0, _) => dst[di..di + 3].fill(255),
                        (_, 0) => {}
                        (_, _) => {
                            for c in 0..3 {
                                dst[di + c] = !dst[di + c];
                            }
                        }
                    }
                }
                CursorKind::MaskedColor => {
                    let si = (cy * shape.pitch + cx * 4) as usize;
                    let Some(src) = shape.data.get(si..si + 4) else {
                        continue;
                    };
                    if src[3] == 0 {
                        dst[di..di + 3].copy_from_slice(&src[..3]);
                    } else {
                        for c in 0..3 {
                            dst[di + c] ^= src[c];
                        }
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 4: Declare the module**

In `crates/castr-capture-win/src/lib.rs`, add `pub mod cursor;` after `pub mod dxgi;`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -q -p castr-capture-win cursor::`
Expected: PASS, 10 tests.

If `masked_colour_treats_alpha_as_a_switch_not_a_blend` fails on the second pixel, check the XOR is against the destination and not the source: `245 == !10`, `235 == !20`, `225 == !30`.

- [ ] **Step 6: Check clippy**

Run: `cargo clippy -q -p castr-capture-win --tests`
Expected: no warnings. The project's Linux CI runs clippy with `-D warnings`.

- [ ] **Step 7: Commit**

```bash
git add crates/castr-capture-win/src/cursor.rs crates/castr-capture-win/src/lib.rs
git commit -m "feat(capture): cursor shapes and the three blend formats"
```

---

### Task 2: Capture the cursor and composite it

**Files:**
- Modify: `crates/castr-capture-win/src/cursor.rs`, `crates/castr-capture-win/src/dxgi.rs`, `crates/castr-capture-win/Cargo.toml`

**Interfaces:**
- Consumes: `cursor::{CursorShape, CursorKind, blend}` from Task 1.
- Produces: `cursor::CursorCache` with `new() -> Self`, `update(&mut self, x: i32, y: i32, visible: bool, shape: Option<CursorShape>)`, and `draw(&self, dst: &mut [u8], width: u32, height: u32, stride: u32)`. `DesktopCapture::next_frame` keeps its signature and now returns frames with the cursor drawn in.

- [ ] **Step 1: Write the failing cache tests**

Add to the test module in `crates/castr-capture-win/src/cursor.rs`:

```rust
    #[test]
    fn the_cache_keeps_the_last_shape_when_none_is_offered() {
        // Windows sends a shape only when it changes, which is rarely. A cache
        // that forgets makes the cursor flicker out on almost every frame.
        let mut c = CursorCache::new();
        c.update(0, 0, true, Some(one_red_pixel()));
        c.update(2, 2, true, None);
        let mut dst = canvas(0, 0, 0);
        c.draw(&mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 2, 2), (0, 0, 255), "moved, same shape");
    }

    #[test]
    fn the_cache_draws_nothing_before_the_first_shape_arrives() {
        let mut c = CursorCache::new();
        c.update(1, 1, true, None);
        let mut dst = canvas(9, 9, 9);
        c.draw(&mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 1, 1), (9, 9, 9));
    }

    #[test]
    fn an_invisible_cursor_is_not_drawn() {
        // Full-screen games and video players hide the cursor; drawing it
        // anyway would be conspicuous.
        let mut c = CursorCache::new();
        c.update(1, 1, true, Some(one_red_pixel()));
        c.update(1, 1, false, None);
        let mut dst = canvas(9, 9, 9);
        c.draw(&mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 1, 1), (9, 9, 9));
    }

    #[test]
    fn the_hotspot_is_subtracted_so_the_tip_lands_where_reported() {
        let mut c = CursorCache::new();
        let shape = CursorShape {
            hotspot_x: 1,
            hotspot_y: 1,
            ..one_red_pixel()
        };
        c.update(2, 2, true, Some(shape));
        let mut dst = canvas(0, 0, 0);
        c.draw(&mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 1, 1), (0, 0, 255), "drawn one up and one left");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -q -p castr-capture-win cursor::`
Expected: FAIL to compile — `CursorCache` does not exist.

- [ ] **Step 3: Write the cache**

Add to `crates/castr-capture-win/src/cursor.rs`, above the test module:

```rust
/// The last shape, position and visibility reported by the duplication API.
///
/// A shape arrives only when it changes, so it is held until replaced;
/// everything else is refreshed per frame.
#[derive(Default)]
pub struct CursorCache {
    shape: Option<CursorShape>,
    x: i32,
    y: i32,
    visible: bool,
}

impl CursorCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, x: i32, y: i32, visible: bool, shape: Option<CursorShape>) {
        self.x = x;
        self.y = y;
        self.visible = visible;
        if let Some(s) = shape {
            self.shape = Some(s);
        }
    }

    /// Draws the cursor into a BGRA frame, hotspot already accounted for.
    pub fn draw(&self, dst: &mut [u8], width: u32, height: u32, stride: u32) {
        if !self.visible {
            return;
        }
        let Some(s) = &self.shape else {
            return;
        };
        blend(
            s,
            self.x - s.hotspot_x,
            self.y - s.hotspot_y,
            dst,
            width,
            height,
            stride,
        );
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -q -p castr-capture-win cursor::`
Expected: PASS, 14 tests.

- [ ] **Step 5: Add the windows feature for pointer shapes**

The pointer-shape structures live behind a feature already available on the
existing dependency. In `crates/castr-capture-win/Cargo.toml`, confirm
`"Win32_Graphics_Dxgi"` and `"Win32_Graphics_Dxgi_Common"` are present — they
already are — and build to check nothing further is needed:

Run: `cargo build -q -p castr-capture-win`
Expected: success. If `DXGI_OUTDUPL_POINTER_SHAPE_INFO` or
`DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME` are reported as unresolved, add
the feature the compiler names to the `windows` features list and no others.

- [ ] **Step 6: Wire the capture**

In `crates/castr-capture-win/src/dxgi.rs`, add to the imports:

```rust
use crate::cursor::{CursorCache, CursorKind, CursorShape};
```

Add a field to `DesktopCapture`:

```rust
    cursor: CursorCache,
```

and initialise it with `cursor: CursorCache::new(),` in `new`.

In `next_frame`, after `AcquireNextFrame` succeeds and before the closure that
copies the texture, read the pointer state. `info` is the
`DXGI_OUTDUPL_FRAME_INFO` already being filled:

```rust
        // The shape is sent only when it changes, so a zero size here means
        // "reuse what you have", not "no cursor".
        let shape = if info.PointerShapeBufferSize > 0 {
            let mut buf = vec![0u8; info.PointerShapeBufferSize as usize];
            let mut needed = 0u32;
            let mut si = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
            // SAFETY: buf is sized by PointerShapeBufferSize, and needed and
            // si are valid out-pointers we own.
            let got = unsafe {
                self.dup.GetFramePointerShape(
                    buf.len() as u32,
                    buf.as_mut_ptr() as *mut _,
                    &mut needed,
                    &mut si,
                )
            };
            match got {
                Ok(()) => {
                    let kind = match DXGI_OUTDUPL_POINTER_SHAPE_TYPE(si.Type as i32) {
                        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME => {
                            Some(CursorKind::Monochrome)
                        }
                        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR => Some(CursorKind::Color),
                        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR => {
                            Some(CursorKind::MaskedColor)
                        }
                        // A shape type we do not know how to draw is skipped
                        // rather than guessed at.
                        _ => None,
                    };
                    kind.map(|kind| CursorShape {
                        kind,
                        width: si.Width,
                        height: si.Height,
                        pitch: si.Pitch,
                        hotspot_x: si.HotSpot.x,
                        hotspot_y: si.HotSpot.y,
                        data: buf,
                    })
                }
                Err(e) => {
                    // Keep the cached shape: a stale cursor beats none.
                    tracing::debug!("cursor shape unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };
        self.cursor.update(
            info.PointerPosition.Position.x,
            info.PointerPosition.Position.y,
            info.PointerPosition.Visible.as_bool(),
            shape,
        );
```

Then, inside the closure, after `data` is built from the mapped memory and
before the `RawFrame` is constructed, composite:

```rust
            // The desktop texture never contains the cursor; Windows hands it
            // over separately, so it is drawn here rather than by the
            // receiver, which keeps it consistent with the picture.
            self.cursor
                .draw(&mut data, self.width, self.height, stride);
```

Note the closure currently borrows `self` immutably. `data` is a local `Vec`,
so drawing into it needs only `&self.cursor`; if the borrow checker objects,
move the `draw` call to after the closure returns, operating on
`frame.data` before `Ok(Some(frame))`.

The `windows` imports need `DXGI_OUTDUPL_POINTER_SHAPE_INFO` and the three
`DXGI_OUTDUPL_POINTER_SHAPE_TYPE_*` constants; `use windows::Win32::Graphics::Dxgi::*;`
is already in the file and should cover them.

- [ ] **Step 7: Add the hardware test**

Add to the existing `tests` module at the bottom of `dxgi.rs`, beside the
current `#[ignore]`d capture test:

```rust
    /// Needs an interactive desktop with the cursor over the primary screen.
    /// Run: cargo test -p castr-capture-win -- --ignored
    #[test]
    #[ignore]
    fn captures_a_frame_that_is_not_uniformly_blank() {
        let mut cap = DesktopCapture::new(0).unwrap();
        // The first frame after startup is often empty; take several.
        let mut frame = None;
        for _ in 0..30 {
            if let Ok(Some(f)) = cap.next_frame(200, 0) {
                frame = Some(f);
            }
        }
        let f = frame.expect("no frame captured");
        let first = &f.data[..4];
        assert!(
            f.data.chunks_exact(4).any(|p| p != first),
            "the captured frame has no variation at all"
        );
    }
```

This asserts the capture path still produces a sane frame with compositing in
it. It cannot assert the cursor specifically without knowing where the mouse
is; the frame dump in Task 5 is what proves the cursor visually.

- [ ] **Step 8: Verify**

```bash
cargo test -q -p castr-capture-win
cargo clippy -q -p castr-capture-win --tests
cargo test -q --workspace
```
Expected: all pass, no clippy warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/castr-capture-win/src/cursor.rs crates/castr-capture-win/src/dxgi.rs crates/castr-capture-win/Cargo.toml
git commit -m "feat(capture): draw the mouse cursor into the captured frame"
```

---

### Task 3: The receiver asks for lost delta fragments

**Files:**
- Modify: `crates/castr-media/src/jitter.rs`, `crates/castr-proto/src/reassemble.rs`, `crates/castr-receiver/src/pipeline.rs`

**Interfaces:**
- Consumes: nothing from Tasks 1-2.
- Produces: `jitter::GAP_WAIT_US` becomes `pub`. `Reassembler::tick` changes signature to `tick(&mut self, now_us: u64, rtt_us: u64, repair_window_us: u64) -> Vec<Nack>`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module at the bottom of `crates/castr-proto/src/reassemble.rs`.
That module already has a `frames(p: &mut Packetizer, keyframe: bool, data: &[u8])`
helper producing fragmented datagrams; use it the way the existing tests do.

```rust
    #[test]
    fn a_delta_missing_a_fragment_is_nacked_while_a_repair_could_still_land() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let f = frames(&mut p, false, &vec![7u8; 400]);
        assert!(f.len() > 1, "the test needs a fragmented frame");
        // Everything but the last fragment.
        for d in &f[..f.len() - 1] {
            r.push(d, 0).unwrap();
        }
        let nacks = r.tick(1_000, 5_000, 150_000);
        assert_eq!(nacks.len(), 1, "the delta is asked for: {nacks:?}");
        assert_eq!(nacks[0].missing, vec![(f.len() - 1) as u16]);
    }

    #[test]
    fn a_delta_whose_repair_could_not_arrive_in_time_is_not_nacked() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let f = frames(&mut p, false, &vec![7u8; 400]);
        for d in &f[..f.len() - 1] {
            r.push(d, 0).unwrap();
        }
        // 140 ms gone of a 150 ms window, and the round trip is 30 ms: the
        // repair would arrive after the frame was needed, so asking wastes
        // upstream bandwidth on a link that has just proved it is lossy.
        let nacks = r.tick(140_000, 30_000, 150_000);
        assert!(nacks.is_empty(), "{nacks:?}");
    }

    #[test]
    fn a_keyframe_is_still_nacked_after_the_repair_window_has_passed() {
        // Without a keyframe nothing decodes at all, so a late one is still
        // worth asking for; the deadline applies to deltas only.
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let f = frames(&mut p, true, &vec![7u8; 400]);
        for d in &f[..f.len() - 1] {
            r.push(d, 0).unwrap();
        }
        let nacks = r.tick(140_000, 30_000, 150_000);
        assert_eq!(nacks.len(), 1, "{nacks:?}");
    }

    #[test]
    fn a_complete_delta_is_never_nacked() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        for d in frames(&mut p, false, &vec![7u8; 400]) {
            r.push(&d, 0).unwrap();
        }
        assert!(r.tick(1_000, 5_000, 150_000).is_empty());
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -q -p castr-proto reassemble`
Expected: FAIL to compile — `tick` takes one argument, not three.

- [ ] **Step 3: Change `tick`**

In `crates/castr-proto/src/reassemble.rs`, replace the `tick` method:

```rust
    /// Frames still missing fragments, as NACKs to send.
    ///
    /// `rtt_us` and `repair_window_us` decide whether asking is still worth
    /// it for a delta: past the point where a repair could arrive before the
    /// receiver needs the frame, the request is wasted upstream bandwidth on
    /// a link that has just demonstrated it is lossy. Keyframes ignore the
    /// deadline, because without one nothing decodes at all.
    pub fn tick(&mut self, now_us: u64, rtt_us: u64, repair_window_us: u64) -> Vec<Nack> {
        let mut nacks = Vec::new();
        let mut expired = Vec::new();
        for (&fnum, p) in self.partial.iter() {
            let age = now_us.saturating_sub(p.first_seen_us);
            if age > self.max_age_us {
                expired.push(fnum);
                self.lost += (p.parts.len() - p.received) as u64;
                continue;
            }
            let in_time = now_us + rtt_us + DECODE_MARGIN_US
                < p.first_seen_us + repair_window_us;
            if !p.keyframe && !in_time {
                continue;
            }
            let missing: Vec<u16> = p
                .parts
                .iter()
                .enumerate()
                .filter(|(_, x)| x.is_none())
                .map(|(i, _)| i as u16)
                .collect();
            nacks.push(Nack {
                frame_number: fnum,
                missing,
            });
        }
        for f in expired {
            self.partial.remove(&f);
        }
        nacks
    }
```

Note the `continue` after the expiry branch: the original used `else if`, and
without the `continue` an expired frame would also be NACKed.

Add the constant near the top of the file:

```rust
/// Allowed for decoding and presenting a repaired frame, on top of the round
/// trip, when deciding whether a repair can still arrive in time.
const DECODE_MARGIN_US: u64 = 10_000;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -q -p castr-proto`
Expected: PASS. Any pre-existing test calling `tick` with one argument must be
updated to pass `rtt_us` and `repair_window_us`; give them `5_000` and
`150_000` unless the test is specifically about expiry, in which case keep its
timings and add the two arguments.

- [ ] **Step 5: Export the window the receiver needs**

In `crates/castr-media/src/jitter.rs`, make the constant public:

```rust
/// How long the jitter buffer waits for a missing frame before giving up on
/// the GOP. It is also the window inside which a repair is still useful, so
/// the receiver passes it to the reassembler.
pub const GAP_WAIT_US: u64 = 150_000;
```

- [ ] **Step 6: Pass the real values at the call site**

In `crates/castr-receiver/src/pipeline.rs`, in the `tick` arm around line 913,
replace the two `tick` calls:

```rust
                // Audio frames are never fragmented, so its reassembler is only
                // ticked to expire partials; its NACKs are meaningless.
                let rtt_us = link.rtt().as_micros() as u64;
                let _ = audio_reasm.tick(now_us(cfg.start), rtt_us, 0);
                let nacks = video_reasm.tick(
                    now_us(cfg.start),
                    rtt_us,
                    castr_media::jitter::GAP_WAIT_US,
                );
```

Check the import path for `jitter` in that file and use whatever form matches
the existing style — the crate is already a dependency.

- [ ] **Step 7: Verify**

```bash
cargo test -q --workspace
cargo clippy -q --workspace --tests
```
Expected: pass, no new warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/castr-media/src/jitter.rs crates/castr-proto/src/reassemble.rs crates/castr-receiver/src/pipeline.rs
git commit -m "feat(proto): ask for lost delta fragments while a repair can still arrive"
```

---

### Task 4: The sender honours delta repairs

**Files:**
- Modify: `crates/castr-net/src/retransmit.rs`
- Create: `crates/castr-net/tests/repair.rs`

**Interfaces:**
- Consumes: `Reassembler::tick(now_us, rtt_us, repair_window_us)` from Task 3.
- Produces: `RetransmitBuffer::lookup(&mut self, nack: &Nack, now_us: u64) -> Vec<Bytes>` — the `frame_interval_us` parameter is removed.

- [ ] **Step 1: Write the failing unit test**

Add to the `tests` module in `crates/castr-net/src/retransmit.rs`:

```rust
    #[test]
    fn delta_fragments_are_resent_for_as_long_as_they_are_held() {
        // The receiver decides whether a repair is worth having: it is the
        // only side that knows its playout deadline. The sender's job is to
        // still have the fragment.
        let mut b = RetransmitBuffer::new(500_000);
        b.record(10, false, frags(4), 0);
        let nack = Nack {
            frame_number: 10,
            missing: vec![2],
        };
        let out = b.lookup(&nack, 100_000);
        assert_eq!(out.len(), 1, "a 100 ms old delta is still resent");
    }

    #[test]
    fn fragments_are_dropped_once_they_age_out() {
        let mut b = RetransmitBuffer::new(500_000);
        b.record(10, false, frags(4), 0);
        let nack = Nack {
            frame_number: 10,
            missing: vec![2],
        };
        assert!(b.lookup(&nack, 600_000).is_empty(), "past the retention window");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -q -p castr-net retransmit`
Expected: FAIL — `lookup` takes a `frame_interval_us` argument, and the first
test's delta would be refused for being older than one interval.

- [ ] **Step 3: Delete the rule**

In `crates/castr-net/src/retransmit.rs`, replace `lookup`:

```rust
    /// Fragments to resend for this NACK, or empty if the frame is unknown or
    /// has aged out.
    ///
    /// There is deliberately no rule here about deltas: the receiver decides
    /// whether a repair can still arrive in time, because it is the only side
    /// that knows its own playout deadline, its mode and the round trip. A
    /// second opinion here could only ever contradict it.
    pub fn lookup(&mut self, nack: &Nack, now_us: u64) -> Vec<Bytes> {
        self.prune(now_us);
        let Some(sent) = self
            .frames
            .iter()
            .find(|s| s.frame_number == nack.frame_number)
        else {
            return Vec::new();
        };
        nack.missing
            .iter()
            .filter_map(|&i| sent.fragments.get(i as usize).cloned())
            .collect()
    }
```

The `keyframe` field on `Sent` is now unused by `lookup`. Leave the field and
the `record` parameter as they are — `record`'s callers pass it and it costs
nothing — but if clippy reports it as dead, add `#[allow(dead_code)]` with a
comment saying it is kept for the log and for future policy rather than
deleting a field the caller still supplies.

- [ ] **Step 4: Update the call site**

In `crates/castr-sender/src/cast.rs` around line 625, drop the third argument:

```rust
                        NackEv::Nack(Ok(nack)) => for f in rtx.lookup(&nack, now) { let _ = link.send_datagram(f); },
```

If `frame_interval_us` becomes unused in that function, remove its binding
too; if it is used elsewhere in the same scope, leave it.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -q -p castr-net`
Expected: PASS. Existing tests naming `frame_interval_us` need the argument
removed; the test asserting a delta older than one interval is refused now
asserts the opposite and should be renamed to say so.

- [ ] **Step 6: Write the end-to-end repair test**

Create `crates/castr-net/tests/repair.rs`:

```rust
//! A lost delta fragment is repaired end to end.
//!
//! This is the behaviour the two one-line rule deletions exist to produce, so
//! it is asserted against both halves at once rather than trusting each in
//! isolation.

use castr_net::retransmit::RetransmitBuffer;
use castr_proto::{Packetizer, Reassembler, STREAM_VIDEO};

#[test]
fn a_dropped_delta_fragment_is_repaired_and_the_frame_completes() {
    let mut p = Packetizer::new();
    let mut sender_rtx = RetransmitBuffer::new(500_000);
    let mut receiver = Reassembler::new(500_000);

    let payload = vec![42u8; 4000];
    let frags = p.packetize(STREAM_VIDEO, false, 1_000, &payload, 1200);
    assert!(frags.len() > 2, "the test needs a fragmented frame");
    sender_rtx.record(p.last_frame_number(), false, frags.clone(), 0);

    // Everything except fragment 1 reaches the receiver.
    for (i, f) in frags.iter().enumerate() {
        if i == 1 {
            continue;
        }
        assert!(
            receiver.push(f, 0).unwrap().is_none(),
            "the frame cannot complete while a fragment is missing"
        );
    }

    // The receiver asks, well inside the repair window.
    let nacks = receiver.tick(1_000, 4_000, 150_000);
    assert_eq!(nacks.len(), 1, "{nacks:?}");
    assert_eq!(nacks[0].missing, vec![1]);

    // The sender answers, and the frame completes.
    let resent = sender_rtx.lookup(&nacks[0], 2_000);
    assert_eq!(resent.len(), 1);
    let done = receiver
        .push(&resent[0], 2_000)
        .unwrap()
        .expect("the repaired fragment completes the frame");
    assert_eq!(done.data, payload, "the frame is byte-for-byte intact");
    assert!(!done.keyframe, "and it is still a delta, not a keyframe");
}
```

If `Packetizer`, `Reassembler`, `STREAM_VIDEO` or `last_frame_number` are not
exported at those paths, check `crates/castr-proto/src/lib.rs` for the real
ones and use those; do not add new `pub use` lines to make the test compile.

- [ ] **Step 7: Run it**

Run: `cargo test -q -p castr-net --test repair`
Expected: PASS.

- [ ] **Step 8: Verify the workspace**

```bash
cargo test -q --workspace
cargo clippy -q --workspace --tests
bash scripts/pi/test-linux.sh
bash scripts/pi/build-pi.sh
```
Expected: all green. The last two use Docker and take several minutes; they
matter because `castr-proto` and `castr-media` are compiled into the Pi
receiver.

- [ ] **Step 9: Commit**

```bash
git add crates/castr-net/src/retransmit.rs crates/castr-net/tests/repair.rs crates/castr-sender/src/cast.rs
git commit -m "feat(net): resend a delta fragment whenever the receiver still wants it"
```

---

### Task 5: Verification

**Files:**
- Create: `docs/superpowers/verification/2026-09-03-castr-cast-quality-e2e.md`
- Modify: `README.md`

**Interfaces:**
- Consumes: everything above.
- Produces: the verification record.

Some steps here need a person at the machines. Those are marked, and must be
recorded as NOT RUN with the reason if nobody is available — never written up
as though they passed.

- [ ] **Step 1: Prove the cursor is in the frame**

The **receiver** dumps its rendered output when `CASTR_DUMP_FRAME=<path>` is
set — see `crates/castr-receiver/src/render.rs:167`. That is the better proof
than dumping at the sender, because it shows the cursor survived capture,
encode, the network and decode. The Pi hardening verification used the same
mechanism.

Capture a dump with the mouse over a known position on a **synthetic test
pattern**, never personal desktop content:

```
powershell -NoProfile -ExecutionPolicy Bypass -File "C:/Users/SETHSA~1/AppData/Local/Temp/claude/D--miracast/2b4241f0-4dcd-4e6e-89b5-6550c719ac5e/scratchpad/testpattern.ps1"
```

Record the image and state plainly whether the cursor appears, at the right
place, with the right shape. Do this for a colour cursor (drag a file) and a
monochrome one (an I-beam over a text field), since they take different code
paths.

- [ ] **Step 2: Count the hitches, before and after**

Cast for five minutes to the Pi in game mode with the receiver's log captured,
and count `decode error` and keyframe-request lines. Compare against the same
run on `master` before this branch. The claim to test is that lost-fragment
stalls fall sharply; record the actual numbers either way, including if they
do not.

- [ ] **Step 3: Measure lip sync (needs a person)**

Play something with a sharp, simultaneous flash and click — a clapperboard
video works. Film the television at 60 fps and count frames between the flash
and the click, in both game and quality modes.

Judge against ITU-R BT.1359: audio leading video by more than **45 ms**, or
lagging by more than **125 ms**, is where viewers notice. Inside that band,
record the number and close the question; outside it, say so plainly — that
becomes the evidence for a separate sub-project, and this one does not grow to
absorb it.

- [ ] **Step 4: Measure what the cursor costs**

From the sender's `perf:` or bitrate log lines, record the encoded bitrate
with the cursor still versus moving continuously, over a minute each on the
same content. This is the one cost of compositing and it should be measured
rather than assumed.

- [ ] **Step 5: Write the document**

Create `docs/superpowers/verification/2026-09-03-castr-cast-quality-e2e.md` in
the same shape as `docs/superpowers/verification/2026-09-03-castr-miracast-resilience-e2e.md`:
a summary table with PASS / FAIL / NOT RUN per numbered step, the commands and
their real output, and a closing section naming everything that did not work or
was not run.

- [ ] **Step 6: Update the README**

In `README.md`, remove these two lines from "Known gaps", since they are what
this branch fixes:

```
- The mouse cursor is not composited into the cast yet.
- Only keyframes are NACK-repaired. A delta frame that loses a fragment costs
  a 150 ms hold and a fresh keyframe; on a Pi 3 over Ethernet that happens a
  few times a minute.
```

Replace them with a single line stating what is true after this branch, and
add the lip-sync measurement from step 3 as a stated figure if it was taken —
or say it has not been measured if it was not.

- [ ] **Step 7: Commit**

```bash
git add docs/superpowers/verification/2026-09-03-castr-cast-quality-e2e.md README.md
git commit -m "docs: cast quality end-to-end verification"
```

---

## After all tasks

**REQUIRED SUB-SKILL:** Use superpowers:finishing-a-development-branch.
