// Puts assets/icon.ico into the Windows exe (Explorer, taskbar, Alt-Tab). Other
// platforms do nothing here. (From hotfire.)
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        #[cfg(windows)]
        {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            res.set("ProductName", "Multipath");
            res.set("FileDescription", "Multipath: HF channel simulator for WAV files");
            if let Err(e) = res.compile() {
                // A missing resource compiler must not stop the build: the exe just has no icon.
                println!("cargo:warning=could not embed the exe icon: {e}");
            }
        }
    }
}
