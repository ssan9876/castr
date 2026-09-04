//! Cursor shapes, and drawing one into a BGRA frame.
//!
//! Windows excludes the cursor from the duplicated desktop texture and hands
//! it over separately, in three formats — two of which predate alpha
//! channels. This module is deliberately free of Windows types so all three
//! can be tested anywhere: the arrow and the text I-beam are monochrome, and
//! getting their mask rules wrong renders the text cursor as a black
//! rectangle.

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

/// The last shape, position and visibility reported by the duplication API.
///
/// Neither half arrives every frame: a shape comes only when it changes, and a
/// position only on a frame that carries a mouse update. Both are therefore
/// held until something replaces them.
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

    /// `position` is `Some((x, y, visible))` only for a frame that reported a
    /// mouse update; `None` means "nothing new", not "no cursor".
    pub fn update(&mut self, position: Option<(i32, i32, bool)>, shape: Option<CursorShape>) {
        if let Some((x, y, visible)) = position {
            self.x = x;
            self.y = y;
            self.visible = visible;
        }
        if let Some(s) = shape {
            self.shape = Some(s);
        }
    }

    /// Draws the cursor into a BGRA frame.
    ///
    /// The position is used as it arrived. The duplication API reports where
    /// the pointer bitmap's top-left corner sits, not where its hotspot does,
    /// so subtracting the hotspot here would shift the cursor up and left by
    /// it - measured on hardware as (392,211) reported for an I-beam whose
    /// hotspot is (8,9) parked at (400,220).
    pub fn draw(&self, dst: &mut [u8], width: u32, height: u32, stride: u32) {
        if !self.visible {
            return;
        }
        let Some(s) = &self.shape else {
            return;
        };
        blend(s, self.x, self.y, dst, width, height, stride);
    }
}

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
    fn blend_ignores_the_hotspot_because_the_caller_applies_it() {
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
                data: [0_u8, 0, 255, 255][..].repeat(4),
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

    #[test]
    fn the_cache_keeps_the_last_shape_when_none_is_offered() {
        // Windows sends a shape only when it changes, which is rarely. A cache
        // that forgets makes the cursor flicker out on almost every frame.
        let mut c = CursorCache::new();
        c.update(Some((0, 0, true)), Some(one_red_pixel()));
        c.update(Some((2, 2, true)), None);
        let mut dst = canvas(0, 0, 0);
        c.draw(&mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 2, 2), (0, 0, 255), "moved, same shape");
    }

    #[test]
    fn the_cache_draws_nothing_before_the_first_shape_arrives() {
        let mut c = CursorCache::new();
        c.update(Some((1, 1, true)), None);
        let mut dst = canvas(9, 9, 9);
        c.draw(&mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 1, 1), (9, 9, 9));
    }

    #[test]
    fn an_invisible_cursor_is_not_drawn() {
        // Full-screen games and video players hide the cursor; drawing it
        // anyway would be conspicuous.
        let mut c = CursorCache::new();
        c.update(Some((1, 1, true)), Some(one_red_pixel()));
        c.update(Some((1, 1, false)), None);
        let mut dst = canvas(9, 9, 9);
        c.draw(&mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 1, 1), (9, 9, 9));
    }

    #[test]
    fn a_frame_with_no_mouse_update_keeps_the_last_position() {
        // The duplication API fills in a pointer position only on a frame that
        // carries a mouse update. Treating the empty field on every other frame
        // as real would put the cursor at the origin and mark it hidden, so a
        // still cursor would vanish a frame after it stopped moving.
        let mut c = CursorCache::new();
        c.update(Some((2, 2, true)), Some(one_red_pixel()));
        c.update(None, None);
        let mut dst = canvas(0, 0, 0);
        c.draw(&mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 2, 2), (0, 0, 255), "still there, still visible");
    }

    #[test]
    fn a_shape_still_lands_on_a_frame_with_no_mouse_update() {
        // Shape and position arrive independently: a shape change with no
        // movement must not be dropped along with the absent position.
        let mut c = CursorCache::new();
        c.update(Some((2, 2, true)), None);
        c.update(None, Some(one_red_pixel()));
        let mut dst = canvas(0, 0, 0);
        c.draw(&mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 2, 2), (0, 0, 255), "shape cached without a move");
    }

    #[test]
    fn the_reported_position_is_the_bitmap_corner_not_the_hotspot() {
        // The duplication API has already applied the hotspot: it reports
        // where the bitmap goes. Applying it a second time here would drag
        // every cursor up and to the left by its own hotspot.
        let mut c = CursorCache::new();
        let shape = CursorShape {
            hotspot_x: 1,
            hotspot_y: 1,
            ..one_red_pixel()
        };
        c.update(Some((2, 2, true)), Some(shape));
        let mut dst = canvas(0, 0, 0);
        c.draw(&mut dst, 4, 4, 16);
        assert_eq!(px(&dst, 2, 2), (0, 0, 255), "drawn where it was reported");
    }
}
