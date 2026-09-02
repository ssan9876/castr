use crate::codec::{PixelFormat, RawFrame};

#[inline]
fn rgb_to_y(r: i32, g: i32, b: i32) -> u8 {
    (16 + ((66 * r + 129 * g + 25 * b + 128) >> 8)).clamp(16, 235) as u8
}
#[inline]
fn rgb_to_u(r: i32, g: i32, b: i32) -> u8 {
    (128 + ((-38 * r - 74 * g + 112 * b + 128) >> 8)).clamp(16, 240) as u8
}
#[inline]
fn rgb_to_v(r: i32, g: i32, b: i32) -> u8 {
    (128 + ((112 * r - 94 * g - 18 * b + 128) >> 8)).clamp(16, 240) as u8
}

/// Returns (Y plane, U plane, V plane) at 4:2:0.
fn bgra_planes(src: &[u8], width: u32, height: u32, stride: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    assert!(
        width.is_multiple_of(2) && height.is_multiple_of(2),
        "dimensions must be even"
    );
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; w * h / 4];
    let mut v = vec![0u8; w * h / 4];
    for row in 0..h {
        for col in 0..w {
            let p = row * s + col * 4;
            let (b, g, r) = (src[p] as i32, src[p + 1] as i32, src[p + 2] as i32);
            y[row * w + col] = rgb_to_y(r, g, b);
        }
    }
    for row in (0..h).step_by(2) {
        for col in (0..w).step_by(2) {
            let (mut rs, mut gs, mut bs) = (0, 0, 0);
            for (dr, dc) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let p = (row + dr) * s + (col + dc) * 4;
                bs += src[p] as i32;
                gs += src[p + 1] as i32;
                rs += src[p + 2] as i32;
            }
            let (r, g, b) = (rs / 4, gs / 4, bs / 4);
            let ci = (row / 2) * (w / 2) + col / 2;
            u[ci] = rgb_to_u(r, g, b);
            v[ci] = rgb_to_v(r, g, b);
        }
    }
    (y, u, v)
}

pub fn bgra_to_i420(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (mut y, u, v) = bgra_planes(src, width, height, stride);
    y.extend_from_slice(&u);
    y.extend_from_slice(&v);
    y
}

pub fn bgra_to_nv12(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (mut y, u, v) = bgra_planes(src, width, height, stride);
    for (a, b) in u.iter().zip(v.iter()) {
        y.push(*a);
        y.push(*b);
    }
    y
}

pub fn nv12_to_i420(src: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    out.extend_from_slice(&src[..w * h]);
    let uv = &src[w * h..];
    out.extend(uv.iter().step_by(2));
    out.extend(uv.iter().skip(1).step_by(2));
    out
}

pub fn convert(frame: &RawFrame, to: PixelFormat) -> RawFrame {
    if frame.format == to {
        return frame.clone();
    }
    let data = match (frame.format, to) {
        (PixelFormat::Bgra, PixelFormat::I420) => {
            bgra_to_i420(&frame.data, frame.width, frame.height, frame.stride)
        }
        (PixelFormat::Bgra, PixelFormat::Nv12) => {
            bgra_to_nv12(&frame.data, frame.width, frame.height, frame.stride)
        }
        (PixelFormat::Nv12, PixelFormat::I420) => {
            nv12_to_i420(&frame.data, frame.width, frame.height)
        }
        (from, to) => panic!("unsupported conversion {from:?} -> {to:?}"),
    };
    RawFrame {
        format: to,
        width: frame.width,
        height: frame.height,
        stride: frame.width,
        data,
        timestamp_us: frame.timestamp_us,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::*;

    fn solid_bgra(w: u32, h: u32, b: u8, g: u8, r: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            v.extend_from_slice(&[b, g, r, 255]);
        }
        v
    }

    #[test]
    fn white_maps_to_limited_range_white() {
        let out = bgra_to_i420(&solid_bgra(4, 2, 255, 255, 255), 4, 2, 16);
        assert_eq!(out.len(), 4 * 2 + 2 * 2);
        assert!(
            out[..8].iter().all(|&y| (234..=236).contains(&y)),
            "Y={:?}",
            &out[..8]
        );
        assert!(out[8..].iter().all(|&c| (127..=129).contains(&c)));
    }

    #[test]
    fn pure_red_has_high_v_low_u() {
        let out = bgra_to_i420(&solid_bgra(2, 2, 0, 0, 255), 2, 2, 8);
        let y = out[0];
        let u = out[4];
        let v = out[5];
        assert!((78..=84).contains(&y), "y={y}");
        assert!(u < 100, "u={u}");
        assert!(v > 220, "v={v}");
    }

    #[test]
    fn stride_larger_than_width_is_respected() {
        let mut src = solid_bgra(2, 2, 255, 255, 255);
        src.splice(8..8, vec![0u8; 8]);
        src.extend_from_slice(&[0u8; 8]);
        let out = bgra_to_i420(&src, 2, 2, 16);
        assert!(out[..4].iter().all(|&y| y > 230));
    }

    #[test]
    fn nv12_and_i420_agree() {
        let src = solid_bgra(4, 4, 30, 200, 90);
        let i420 = bgra_to_i420(&src, 4, 4, 16);
        let nv12 = bgra_to_nv12(&src, 4, 4, 16);
        assert_eq!(nv12.len(), i420.len());
        assert_eq!(&nv12[..16], &i420[..16]);
        assert_eq!(nv12_to_i420(&nv12, 4, 4), i420);
    }

    #[test]
    fn convert_identity_and_bgra_to_targets() {
        let f = RawFrame {
            format: PixelFormat::Bgra,
            width: 2,
            height: 2,
            stride: 8,
            data: solid_bgra(2, 2, 1, 2, 3),
            timestamp_us: 9,
        };
        let same = convert(&f, PixelFormat::Bgra);
        assert_eq!(same.data, f.data);
        let i420 = convert(&f, PixelFormat::I420);
        assert_eq!(i420.format, PixelFormat::I420);
        assert_eq!(i420.stride, 2);
        assert_eq!(i420.timestamp_us, 9);
        assert_eq!(i420.data.len(), 6);
        let nv12 = convert(&f, PixelFormat::Nv12);
        assert_eq!(nv12.data.len(), 6);
    }
}
