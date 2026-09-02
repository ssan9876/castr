use anyhow::{anyhow, Context};
use castr_media::{PixelFormat, RawFrame};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::{Texture, TextureCreator, WindowCanvas};
use sdl2::video::WindowContext;
use sdl2::{EventPump, Sdl};

/// Number of pixel rows in one glyph of the built-in font.
const GLYPH_ROWS: usize = 7;
/// Number of pixel columns in one glyph; each row byte uses bits 4..0.
const GLYPH_COLS: u32 = 5;

/// A 5x7 bitmap font, enough for the overlay strings the receiver shows
/// ("PIN 123456", "WAITING FOR SENDER", "RECONNECTING"). Pulling in a real
/// font rasteriser would break the no-external-assets property of the
/// receiver binary, and the overlays only ever need uppercase letters,
/// digits, space, colon and hyphen.
///
/// Each entry is 7 row bytes, top to bottom; bit 4 is the leftmost pixel,
/// bit 0 the rightmost, so no row may use a bit above the 5th.
const FONT: &[(char, [u8; GLYPH_ROWS])] = &[
    (' ', [0, 0, 0, 0, 0, 0, 0]),
    ('-', [0x00, 0x00, 0x00, 0x0E, 0x00, 0x00, 0x00]),
    (':', [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00]),
    ('0', [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E]),
    ('1', [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E]),
    ('2', [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F]),
    ('3', [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E]),
    ('4', [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02]),
    ('5', [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E]),
    ('6', [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E]),
    ('7', [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08]),
    ('8', [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E]),
    ('9', [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C]),
    ('A', [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11]),
    ('B', [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E]),
    ('C', [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E]),
    ('D', [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E]),
    ('E', [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F]),
    ('F', [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10]),
    ('G', [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F]),
    ('H', [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11]),
    ('I', [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E]),
    ('J', [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C]),
    ('K', [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11]),
    ('L', [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F]),
    ('M', [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11]),
    ('N', [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11]),
    ('O', [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E]),
    ('P', [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10]),
    ('Q', [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D]),
    ('R', [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11]),
    ('S', [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E]),
    ('T', [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04]),
    ('U', [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E]),
    ('V', [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04]),
    ('W', [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11]),
    ('X', [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11]),
    ('Y', [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04]),
    ('Z', [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F]),
];

/// Rows for `c`, or the blank glyph for anything the font does not cover.
fn glyph(c: char) -> &'static [u8; GLYPH_ROWS] {
    let c = c.to_ascii_uppercase();
    FONT.iter()
        .find(|(g, _)| *g == c)
        .map(|(_, rows)| rows)
        .unwrap_or(&FONT[0].1)
}

/// Width in pixels of `text` at `scale`, including the 1-pixel gap between
/// glyphs but not a trailing gap.
fn text_width(text: &str, scale: u32) -> u32 {
    let n = text.chars().count() as u32;
    if n == 0 {
        0
    } else {
        (n * (GLYPH_COLS + 1) - 1) * scale
    }
}

pub struct Renderer {
    pub sdl: Sdl,
    canvas: WindowCanvas,
    creator: TextureCreator<WindowContext>,
    texture: Option<Texture>,
    tex_desc: (u32, u32, PixelFormat),
    overlay: Option<String>,
    events: EventPump,
    base_title: String,
    pulse: u32,
}

impl Renderer {
    pub fn new(title: &str, fullscreen: bool) -> anyhow::Result<Self> {
        let sdl = sdl2::init().map_err(|e| anyhow!(e))?;
        let video = sdl.video().map_err(|e| anyhow!(e))?;
        let mut builder = video.window(title, 1280, 720);
        builder.position_centered().resizable();
        if fullscreen {
            builder.fullscreen_desktop();
        }
        let window = builder.build().context("create window")?;
        let canvas = window
            .into_canvas()
            .accelerated()
            .present_vsync()
            .build()
            .context("create canvas")?;
        let creator = canvas.texture_creator();
        let events = sdl.event_pump().map_err(|e| anyhow!(e))?;
        Ok(Self {
            sdl,
            canvas,
            creator,
            texture: None,
            tex_desc: (0, 0, PixelFormat::I420),
            overlay: None,
            events,
            base_title: title.to_string(),
            pulse: 0,
        })
    }

    pub fn set_overlay(&mut self, text: Option<&str>) {
        self.overlay = text.map(|s| s.to_string());
        let title = match &self.overlay {
            Some(t) => format!("{} - {}", self.base_title, t),
            None => self.base_title.clone(),
        };
        let _ = self.canvas.window_mut().set_title(&title);
    }

    /// Pumps SDL events. Returns true when the user asked to quit (window close or Escape).
    pub fn poll_quit(&mut self) -> bool {
        for ev in self.events.poll_iter() {
            match ev {
                Event::Quit { .. } => return true,
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => return true,
                _ => {}
            }
        }
        false
    }

    fn ensure_texture(&mut self, f: &RawFrame) -> anyhow::Result<()> {
        if self.texture.is_some() && self.tex_desc == (f.width, f.height, f.format) {
            return Ok(());
        }
        let fmt = match f.format {
            PixelFormat::I420 => PixelFormatEnum::IYUV,
            PixelFormat::Nv12 => PixelFormatEnum::NV12,
            PixelFormat::Bgra => PixelFormatEnum::ARGB8888,
        };
        let tex = self
            .creator
            .create_texture_streaming(fmt, f.width, f.height)
            .context("create texture")?;
        self.texture = Some(tex);
        self.tex_desc = (f.width, f.height, f.format);
        Ok(())
    }

    pub fn present(&mut self, f: &RawFrame) -> anyhow::Result<()> {
        self.ensure_texture(f)?;
        let (w, h) = (f.width as usize, f.height as usize);
        let tex = self.texture.as_mut().unwrap();
        match f.format {
            PixelFormat::I420 => {
                let y = &f.data[..w * h];
                let u = &f.data[w * h..w * h + w * h / 4];
                let v = &f.data[w * h + w * h / 4..];
                tex.update_yuv(None, y, w, u, w / 2, v, w / 2)
                    .map_err(|e| anyhow!(e))?;
            }
            PixelFormat::Nv12 => tex.update(None, &f.data, w).map_err(|e| anyhow!(e))?,
            PixelFormat::Bgra => tex
                .update(None, &f.data, f.stride as usize)
                .map_err(|e| anyhow!(e))?,
        }
        self.draw()
    }

    pub fn redraw(&mut self) -> anyhow::Result<()> {
        self.draw()
    }

    /// Draws `text` with the built-in font, one filled rect per lit pixel,
    /// with the top-left of the first glyph at (`x`, `y`).
    fn draw_text(
        &mut self,
        text: &str,
        x: i32,
        y: i32,
        scale: u32,
        color: Color,
    ) -> anyhow::Result<()> {
        self.canvas.set_draw_color(color);
        let mut rects = Vec::new();
        for (i, c) in text.chars().enumerate() {
            let gx = x + (i as u32 * (GLYPH_COLS + 1) * scale) as i32;
            for (row, bits) in glyph(c).iter().enumerate() {
                for col in 0..GLYPH_COLS {
                    if bits & (1 << (GLYPH_COLS - 1 - col)) != 0 {
                        rects.push(Rect::new(
                            gx + (col * scale) as i32,
                            y + (row as u32 * scale) as i32,
                            scale,
                            scale,
                        ));
                    }
                }
            }
        }
        self.canvas.fill_rects(&rects).map_err(|e| anyhow!(e))?;
        Ok(())
    }

    fn draw(&mut self) -> anyhow::Result<()> {
        self.canvas.set_draw_color(Color::RGB(0, 0, 0));
        self.canvas.clear();
        let (ww, wh) = self.canvas.output_size().map_err(|e| anyhow!(e))?;
        if let Some(tex) = &self.texture {
            let (tw, th, _) = self.tex_desc;
            let scale = (ww as f64 / tw as f64).min(wh as f64 / th as f64);
            let dw = (tw as f64 * scale) as u32;
            let dh = (th as f64 * scale) as u32;
            let dst = Rect::new(((ww - dw) / 2) as i32, ((wh - dh) / 2) as i32, dw, dh);
            self.canvas.copy(tex, None, dst).map_err(|e| anyhow!(e))?;
        }
        if let Some(text) = self.overlay.clone() {
            self.pulse = self.pulse.wrapping_add(1);
            self.canvas.set_blend_mode(sdl2::render::BlendMode::Blend);
            self.canvas.set_draw_color(Color::RGBA(0, 0, 0, 140));
            self.canvas
                .fill_rect(Rect::new(0, 0, ww, wh))
                .map_err(|e| anyhow!(e))?;
            let bar_w = ww / 3;
            let bar_y = (wh / 2) as i32 - 4;
            let x = ((self.pulse * 8) % (ww + bar_w)) as i32 - bar_w as i32;
            self.canvas.set_draw_color(Color::RGBA(255, 255, 255, 200));
            self.canvas
                .fill_rect(Rect::new(x, bar_y, bar_w, 8))
                .map_err(|e| anyhow!(e))?;
            // Glyphs about 1/20 of the window height, centred above the bar.
            let text = text.to_uppercase();
            let scale = (wh / 20 / GLYPH_ROWS as u32).max(2);
            let tw = text_width(&text, scale);
            let tx = ((ww as i32 - tw as i32) / 2).max(0);
            let ty = bar_y - (GLYPH_ROWS as u32 * scale) as i32 - 3 * scale as i32;
            self.draw_text(&text, tx, ty, scale, Color::RGBA(255, 255, 255, 235))?;
        }
        self.canvas.present();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_is_seven_rows_of_five_bits() {
        for (c, rows) in FONT {
            assert_eq!(rows.len(), GLYPH_ROWS, "glyph {c:?}");
            for (i, r) in rows.iter().enumerate() {
                assert!(
                    *r < (1 << GLYPH_COLS),
                    "glyph {c:?} row {i} uses bits above the 5th: {r:#04x}"
                );
            }
        }
    }

    #[test]
    fn font_covers_the_overlay_alphabet_without_duplicates() {
        for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 :-".chars() {
            assert!(FONT.iter().any(|(g, _)| *g == c), "missing glyph {c:?}");
        }
        let mut seen: Vec<char> = FONT.iter().map(|(c, _)| *c).collect();
        seen.sort_unstable();
        let n = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), n, "duplicate glyph in FONT");
    }

    #[test]
    fn distinct_characters_have_distinct_glyphs() {
        assert_ne!(glyph('7'), glyph('1'));
        // Lowercase input maps onto the uppercase glyph.
        assert_eq!(glyph('a'), glyph('A'));
        // Unknown characters fall back to the blank glyph.
        assert_eq!(glyph('%'), glyph(' '));
    }

    #[test]
    fn text_width_counts_inter_glyph_gaps() {
        assert_eq!(text_width("", 3), 0);
        assert_eq!(text_width("A", 3), 15);
        assert_eq!(text_width("AB", 3), 33);
    }
}
