//! What monitors are there to cast.
//!
//! `DesktopCapture::new` takes a bare output index and nothing could tell you
//! what any index meant, so choosing a monitor meant a test cast. This lists
//! them.
//!
//! The numbering is the graphics adapter's, not the order Windows shows in
//! display settings, and that does not change here — but a name and a
//! resolution are enough to recognise which is which without casting to find
//! out.

use anyhow::Context;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_MODE_ROTATION_ROTATE270, DXGI_MODE_ROTATION_ROTATE90,
};
use windows::Win32::Graphics::Dxgi::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// What to pass to `DesktopCapture::new`, and what `CASTR_OUTPUT` sets.
    pub index: u32,
    /// The adapter's name for it, like `\\.\DISPLAY1`.
    pub device_name: String,
    pub width: u32,
    pub height: u32,
    /// The desktop's origin is on this monitor, which is what Windows means by
    /// the primary display.
    pub primary: bool,
    /// Windows has this monitor turned. Worth surfacing: the capture path does
    /// not consult rotation, so casting one arrives sideways.
    pub rotated: bool,
}

/// Every monitor attached to the adapter that owns the desktop.
pub fn outputs() -> anyhow::Result<Vec<Output>> {
    // SAFETY: FFI into DXGI; the factory pointer is ours and checked.
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.context("CreateDXGIFactory1")?;
    let mut found = Vec::new();
    let mut adapter_index = 0u32;
    // SAFETY: enumeration ends with an error, which is how DXGI says "no more".
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(adapter_index) } {
        let mut output_index = 0u32;
        // SAFETY: same enumeration contract on the adapter's outputs.
        while let Ok(output) = unsafe { adapter.EnumOutputs(output_index) } {
            // SAFETY: output is a valid COM interface just obtained.
            if let Ok(desc) = unsafe { output.GetDesc() } {
                let r = desc.DesktopCoordinates;
                // An output with no desktop on it cannot be duplicated, so
                // offering it would only produce a cast that fails to start.
                if !desc.AttachedToDesktop.as_bool() {
                    output_index += 1;
                    continue;
                }
                found.push(Output {
                    index: output_index,
                    device_name: String::from_utf16_lossy(&desc.DeviceName)
                        .trim_end_matches('\0')
                        .to_string(),
                    width: (r.right - r.left).unsigned_abs(),
                    height: (r.bottom - r.top).unsigned_abs(),
                    // The primary monitor is the one the desktop's origin sits
                    // on; DXGI does not label it any more directly than that.
                    primary: r.left == 0 && r.top == 0,
                    rotated: matches!(
                        desc.Rotation,
                        DXGI_MODE_ROTATION_ROTATE90 | DXGI_MODE_ROTATION_ROTATE270
                    ),
                });
            }
            output_index += 1;
        }
        // Only the adapter that actually drives monitors is of interest; once
        // one has produced outputs, a second adapter's indices would collide
        // with it and `DesktopCapture` only ever looks at the first.
        if !found.is_empty() {
            break;
        }
        adapter_index += 1;
    }
    Ok(found)
}

/// How to name a monitor in a picker.
pub fn label(o: &Output) -> String {
    let mut s = format!("{}  {}x{}", short_name(&o.device_name), o.width, o.height);
    if o.primary {
        s.push_str("  (primary)");
    }
    if o.rotated {
        s.push_str("  (rotated - the cast will appear sideways)");
    }
    s
}

/// `\\.\DISPLAY1` reads as `DISPLAY1`; the prefix is on every one of them and
/// tells the reader nothing.
fn short_name(device_name: &str) -> &str {
    device_name.rsplit('\\').next().unwrap_or(device_name)
}

/// Which monitor to offer before anyone chooses: the primary, or the first.
pub fn default_index(list: &[Output]) -> u32 {
    list.iter()
        .find(|o| o.primary)
        .or_else(|| list.first())
        .map(|o| o.index)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(index: u32, name: &str, primary: bool) -> Output {
        Output {
            index,
            device_name: name.into(),
            width: 1920,
            height: 1080,
            primary,
            rotated: false,
        }
    }

    #[test]
    fn a_label_names_the_monitor_and_its_size() {
        let l = label(&out(0, r"\\.\DISPLAY1", false));
        assert_eq!(l, "DISPLAY1  1920x1080");
    }

    #[test]
    fn the_primary_says_so() {
        assert!(label(&out(0, r"\\.\DISPLAY1", true)).contains("(primary)"));
    }

    #[test]
    fn a_rotated_monitor_warns_that_the_cast_will_be_sideways() {
        // The capture path ignores rotation, so this is a standing limitation
        // the picker can at least be honest about.
        let o = Output {
            rotated: true,
            ..out(1, r"\\.\DISPLAY2", false)
        };
        assert!(label(&o).contains("sideways"));
    }

    #[test]
    fn a_name_without_the_prefix_survives() {
        assert_eq!(short_name("DISPLAY3"), "DISPLAY3");
    }

    #[test]
    fn the_primary_is_the_default() {
        let list = vec![out(0, r"\\.\DISPLAY1", false), out(1, r"\\.\DISPLAY2", true)];
        assert_eq!(default_index(&list), 1);
    }

    #[test]
    fn without_a_primary_the_first_is_the_default() {
        let list = vec![out(2, r"\\.\DISPLAY1", false), out(3, r"\\.\DISPLAY2", false)];
        assert_eq!(default_index(&list), 2);
    }

    /// Needs real monitors, so it is not part of the ordinary run. Prints what
    /// it found: `cargo test -p castr-capture-win -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn lists_the_real_monitors() {
        let found = outputs().expect("enumerating outputs");
        for o in &found {
            println!("CASTR_OUTPUT={}  {}", o.index, label(o));
        }
        assert!(!found.is_empty(), "a machine with a desktop has an output");
        assert_eq!(
            found.iter().filter(|o| o.primary).count(),
            1,
            "exactly one monitor holds the desktop origin"
        );
    }

    #[test]
    fn an_empty_list_defaults_to_output_zero() {
        // Enumeration failing must not stop a cast: index 0 is what the CLI
        // has always used.
        assert_eq!(default_index(&[]), 0);
    }
}
