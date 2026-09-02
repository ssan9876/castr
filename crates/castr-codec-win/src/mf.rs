use anyhow::Context;
use std::sync::Once;
use windows::core::GUID;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

static START: Once = Once::new();

pub fn mf_startup() -> anyhow::Result<()> {
    let mut result = Ok(());
    START.call_once(|| {
        // SAFETY: CoInitializeEx and MFStartup are one-time process initialization calls
        // guarded by `Once`; no pointers are involved beyond the API's own state.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            result = MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup");
        }
    });
    result
}

pub fn make_sample(data: &[u8], time_us: u64, duration_us: u64) -> anyhow::Result<IMFSample> {
    // SAFETY: `buffer` is a freshly created MF memory buffer; `ptr` is the locked
    // pointer it hands back, valid for `data.len()` bytes until `Unlock`.
    unsafe {
        let buffer = MFCreateMemoryBuffer(data.len() as u32).context("MFCreateMemoryBuffer")?;
        let mut ptr = std::ptr::null_mut();
        buffer.Lock(&mut ptr, None, None).context("buffer Lock")?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        buffer.Unlock().context("buffer Unlock")?;
        buffer.SetCurrentLength(data.len() as u32)?;
        let sample = MFCreateSample().context("MFCreateSample")?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime((time_us * 10) as i64)?;
        sample.SetSampleDuration((duration_us * 10) as i64)?;
        Ok(sample)
    }
}

pub fn read_sample(sample: &IMFSample) -> anyhow::Result<Vec<u8>> {
    // SAFETY: `buffer` is a valid contiguous MF buffer obtained from `sample`;
    // `ptr`/`len` are the locked pointer and length it hands back, valid until `Unlock`.
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .context("ConvertToContiguousBuffer")?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer
            .Lock(&mut ptr, None, Some(&mut len))
            .context("Lock")?;
        let out = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        buffer.Unlock()?;
        Ok(out)
    }
}

pub fn video_type(
    subtype: &GUID,
    w: u32,
    h: u32,
    fps: u32,
    bitrate: Option<u32>,
) -> anyhow::Result<IMFMediaType> {
    // SAFETY: `t` is a freshly created media type COM object; all setters take valid
    // attribute GUIDs and by-value/by-ref arguments per the MF API contract.
    unsafe {
        let t = MFCreateMediaType().context("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        if let Some(b) = bitrate {
            t.SetUINT32(&MF_MT_AVG_BITRATE, b)?;
        }
        if *subtype == MFVideoFormat_H264 {
            t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
        }
        Ok(t)
    }
}

pub fn find_transforms(
    category: GUID,
    input: &GUID,
    output: &GUID,
    hardware: bool,
) -> anyhow::Result<Vec<IMFActivate>> {
    let input_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: *input,
    };
    let output_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: *output,
    };
    let flags = if hardware {
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER
    } else {
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: `MFTEnumEx` writes an array of `count` COM interface pointers into
    // `activates`, allocated by MF with `CoTaskMemAlloc`; we take ownership of each
    // and free the array itself with `CoTaskMemFree`.
    unsafe {
        MFTEnumEx(
            category,
            flags,
            Some(&input_info),
            Some(&output_info),
            &mut activates,
            &mut count,
        )
        .context("MFTEnumEx")?;
        let mut out = Vec::new();
        if !activates.is_null() {
            for i in 0..count as usize {
                if let Some(a) = (*activates.add(i)).take() {
                    out.push(a);
                }
            }
            CoTaskMemFree(Some(activates as *const _));
        }
        Ok(out)
    }
}

pub fn transform_name(activate: &IMFActivate) -> String {
    // SAFETY: `ptr` is populated by `GetAllocatedString` with a `CoTaskMemAlloc`'d
    // string; we convert it and free it with `CoTaskMemFree`.
    unsafe {
        let mut ptr = windows::core::PWSTR::null();
        let mut len = 0u32;
        if activate
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut ptr, &mut len)
            .is_ok()
            && !ptr.is_null()
        {
            let s = ptr.to_string().unwrap_or_default();
            CoTaskMemFree(Some(ptr.0 as *const _));
            s
        } else {
            "unnamed MFT".into()
        }
    }
}
