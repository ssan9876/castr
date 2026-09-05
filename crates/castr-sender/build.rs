fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set("ProductName", "castr sender");
        res.set("FileDescription", "castr screen sender");
        res.set("CompanyName", "castr");
        res.set("LegalCopyright", "MIT licensed");
        // The icon Explorer, the taskbar, the Start Menu shortcut and Add or
        // Remove Programs all show. Committed rather than generated at build
        // time: `scripts/windows/make-icon.ps1` regenerates it when the
        // artwork changes, and an ordinary build never needs to.
        res.set_icon("../../assets/castr.ico");
        let _ = res.compile();
        println!("cargo:rerun-if-changed=../../assets/castr.ico");
    }
}
