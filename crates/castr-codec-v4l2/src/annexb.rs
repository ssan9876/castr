// crates/castr-codec-v4l2/src/annexb.rs
//! Minimal Annex B inspection: enough to reject junk before it reaches the
//! hardware and to recognise keyframes in tests and logs.

use anyhow::bail;

pub fn starts_with_start_code(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1])
}

/// NAL unit types (low 5 bits of the byte after each start code), in order.
pub fn nal_types(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(&b) = data.get(i + 3) {
                out.push(b & 0x1f);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

pub fn has_sps(data: &[u8]) -> bool {
    nal_types(data).contains(&7)
}

pub fn is_idr(data: &[u8]) -> bool {
    nal_types(data).contains(&5)
}

pub fn check_access_unit(data: &[u8]) -> anyhow::Result<()> {
    if data.is_empty() {
        bail!("empty access unit");
    }
    if !starts_with_start_code(data) {
        bail!("access unit is not Annex B (no start code)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 5] = [0, 0, 0, 1, 0x67];
    const PPS: [u8; 5] = [0, 0, 0, 1, 0x68];
    const IDR: [u8; 5] = [0, 0, 0, 1, 0x65];
    const P: [u8; 4] = [0, 0, 1, 0x41];

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn detects_three_and_four_byte_start_codes() {
        assert!(starts_with_start_code(&SPS));
        assert!(starts_with_start_code(&P));
        assert!(!starts_with_start_code(&[0, 0, 0, 0, 1]));
        assert!(!starts_with_start_code(&[]));
        assert!(!starts_with_start_code(&[0x65, 1, 2]));
    }

    #[test]
    fn lists_nal_types_in_order() {
        let au = cat(&[&SPS, &[1, 2, 3], &PPS, &IDR, &[0, 0, 3, 1]]);
        assert_eq!(nal_types(&au), vec![7, 8, 5]);
    }

    #[test]
    fn keyframe_and_parameter_set_detection() {
        let key = cat(&[&SPS, &PPS, &IDR]);
        assert!(has_sps(&key));
        assert!(is_idr(&key));
        assert!(!has_sps(&P));
        assert!(!is_idr(&P));
    }

    #[test]
    fn check_rejects_non_annex_b_and_empty_input() {
        assert!(check_access_unit(&[]).is_err());
        assert!(check_access_unit(&[0x00, 0x00, 0x02, 0x09]).is_err());
        assert!(check_access_unit(&P).is_ok());
    }
}
