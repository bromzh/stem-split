//! Embeds the application icon and version information into the Windows
//! executable, where the file manager and the task bar look for them.

use std::env;

const ICON: &str = "assets/icon.ico";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ICON}");
    // The target platform, not the one the build script itself runs on.
    if env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(ICON);
    if let Err(error) = resource.compile() {
        // The application works without the resource; only the icon of its
        // file is missing. That is not worth failing the build for.
        println!("cargo:warning=the application icon was not embedded: {error}");
    }
}
