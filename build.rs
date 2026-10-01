//! Embeds the application icon and version information into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/vigil.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/vigil.ico")
            .set("FileDescription", "Vigil Task Manager")
            .set("ProductName", "Vigil Task Manager");
        // Missing resource tools (e.g. when cross-checking) must not break the build.
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed Windows resources: {e}");
        }
    }
}
