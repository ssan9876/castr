use anyhow::{anyhow, Context};
use castr_media::{PixelFormat, RawFrame};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::{Texture, TextureCreator, WindowCanvas};
use sdl2::video::WindowContext;
use sdl2::{EventPump, Sdl};

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
        if self.overlay.is_some() {
            self.pulse = self.pulse.wrapping_add(1);
            self.canvas.set_blend_mode(sdl2::render::BlendMode::Blend);
            self.canvas.set_draw_color(Color::RGBA(0, 0, 0, 140));
            self.canvas
                .fill_rect(Rect::new(0, 0, ww, wh))
                .map_err(|e| anyhow!(e))?;
            let bar_w = ww / 3;
            let x = ((self.pulse * 8) % (ww + bar_w)) as i32 - bar_w as i32;
            self.canvas.set_draw_color(Color::RGBA(255, 255, 255, 200));
            self.canvas
                .fill_rect(Rect::new(x, (wh / 2) as i32 - 4, bar_w, 8))
                .map_err(|e| anyhow!(e))?;
        }
        self.canvas.present();
        Ok(())
    }
}
