#[cfg(windows)]
fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("../../assets/icon.ico");
    res.set("FileDescription", "HapLab - Pure Rust HAP Video Studio");
    res.set("ProductName", "HapLab");
    res.set("LegalCopyright", "Copyright (c) 2026 Lee Brown");
    let _ = res.compile();
}

#[cfg(not(windows))]
fn main() {}
