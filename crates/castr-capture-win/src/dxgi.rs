use crate::cursor::{CursorCache, CursorKind, CursorShape};
use anyhow::{bail, Context};
use castr_media::{PixelFormat, RawFrame};
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

pub struct DesktopCapture {
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
    cursor: CursorCache,
}

impl DesktopCapture {
    pub fn new(output_index: u32) -> anyhow::Result<Self> {
        let mut device = None;
        let mut context = None;
        // SAFETY: FFI call into D3D11; output pointers are valid `Option` slots we own.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("D3D11CreateDevice")?;
        }
        let device = device.context("no D3D11 device")?;
        let context = context.context("no D3D11 context")?;
        let dxgi_device: IDXGIDevice = device.cast().context("IDXGIDevice")?;
        // SAFETY: dxgi_device is a valid COM interface obtained above.
        let adapter = unsafe { dxgi_device.GetAdapter() }.context("GetAdapter")?;
        // SAFETY: adapter is a valid COM interface obtained above.
        let output = unsafe { adapter.EnumOutputs(output_index) }.context("EnumOutputs")?;
        let output1: IDXGIOutput1 = output.cast().context("IDXGIOutput1")?;
        // SAFETY: output1 and device are valid COM interfaces obtained above.
        let dup = unsafe { output1.DuplicateOutput(&device) }
            .context("DuplicateOutput (is another app already duplicating?)")?;
        // SAFETY: dup is a valid COM interface.
        let desc = unsafe { dup.GetDesc() };
        let width = desc.ModeDesc.Width & !1;
        let height = desc.ModeDesc.Height & !1;
        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        // SAFETY: device is a valid COM interface; tex_desc is a valid in-pointer, staging a valid out-pointer.
        unsafe { device.CreateTexture2D(&tex_desc, None, Some(&mut staging)) }
            .context("CreateTexture2D staging")?;
        Ok(Self {
            _device: device,
            context,
            dup,
            staging: staging.context("no staging texture")?,
            width,
            height,
            cursor: CursorCache::new(),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn next_frame(
        &mut self,
        timeout_ms: u32,
        timestamp_us: u64,
    ) -> anyhow::Result<Option<RawFrame>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: dup is a valid COM interface; info and resource are valid out-pointers.
        if let Err(e) = unsafe {
            self.dup
                .AcquireNextFrame(timeout_ms, &mut info, &mut resource)
        } {
            if e.code() == DXGI_ERROR_WAIT_TIMEOUT {
                return Ok(None);
            }
            if e.code() == DXGI_ERROR_ACCESS_LOST {
                bail!("desktop duplication access lost");
            }
            return Err(e).context("AcquireNextFrame");
        }
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
        // PointerPosition is filled in only on a frame that carries a mouse
        // update; on every other frame it is zeroed. Reading it regardless
        // would park a still cursor at the origin and call it hidden, which is
        // most of the time - the pointer is stationary far more often than not.
        let position = (info.LastMouseUpdateTime != 0).then(|| {
            (
                info.PointerPosition.Position.x,
                info.PointerPosition.Position.y,
                info.PointerPosition.Visible.as_bool(),
            )
        });
        self.cursor.update(position, shape);
        let result = (|| -> anyhow::Result<RawFrame> {
            let tex: ID3D11Texture2D = resource
                .as_ref()
                .context("no resource")?
                .cast()
                .context("ID3D11Texture2D")?;
            let src_box = D3D11_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: self.width,
                bottom: self.height,
                back: 1,
            };
            // SAFETY: context, staging and tex are valid COM interfaces; src_box describes a region within tex's bounds.
            unsafe {
                self.context.CopySubresourceRegion(
                    &self.staging,
                    0,
                    0,
                    0,
                    0,
                    &tex,
                    0,
                    Some(&src_box),
                )
            };
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            // SAFETY: context and staging are valid COM interfaces; mapped is a valid out-pointer.
            unsafe {
                self.context
                    .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            }
            .context("Map staging")?;
            let stride = mapped.RowPitch;
            let len = (stride * self.height) as usize;
            // SAFETY: mapped.pData points to `len` valid bytes for the duration of the map, per D3D11 Map contract.
            let data =
                unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, len) }.to_vec();
            // SAFETY: staging is the same resource that was mapped above.
            unsafe { self.context.Unmap(&self.staging, 0) };
            Ok(RawFrame {
                format: PixelFormat::Bgra,
                width: self.width,
                height: self.height,
                stride,
                data,
                timestamp_us,
            })
        })();
        // SAFETY: dup is a valid COM interface; ReleaseFrame must be called after AcquireNextFrame regardless of outcome.
        let _ = unsafe { self.dup.ReleaseFrame() };
        let mut frame = result?;
        // The desktop texture never contains the cursor; Windows hands it
        // over separately, so it is drawn here rather than by the receiver,
        // which keeps it consistent with the picture.
        self.cursor
            .draw(&mut frame.data, self.width, self.height, frame.stride);
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs an interactive desktop. Run: cargo test -p castr-capture-win -- --ignored
    #[test]
    #[ignore]
    fn captures_a_frame_with_even_dimensions() {
        let mut cap = DesktopCapture::new(0).unwrap();
        let (w, h) = cap.size();
        assert!(w % 2 == 0 && h % 2 == 0 && w >= 640 && h >= 480);
        let mut got = None;
        for _ in 0..50 {
            if let Some(f) = cap.next_frame(100, 1).unwrap() {
                got = Some(f);
                break;
            }
        }
        let f = got.expect("desktop produced a frame within 5 s");
        assert_eq!(
            (f.width, f.height, f.format),
            (w, h, castr_media::PixelFormat::Bgra)
        );
        assert!(f.stride >= w * 4);
        assert_eq!(f.data.len(), (f.stride * h) as usize);
        assert_eq!(f.timestamp_us, 1);
    }

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
        let (pixels, _) = f.data.as_chunks::<4>();
        let first = pixels[0];
        assert!(
            pixels.iter().any(|p| *p != first),
            "the captured frame has no variation at all"
        );
    }
}
