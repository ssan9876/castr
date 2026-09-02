fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set("ProductName", "castr sender");
        res.set("FileDescription", "castr screen sender");
        let _ = res.compile();
    }
}
