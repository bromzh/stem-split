//! Puts `DirectML.dll` next to test and example binaries.
//!
//! `ort-sys` places the library in `target/<profile>/`, beside the
//! application binary. Unless it is allowed to create symlinks (Windows
//! Developer Mode), it stops there. Binaries in `deps/` and `examples/` would
//! then load the outdated copy that ships with Windows, on which the GPU
//! provider fails to start, and silently run on the CPU instead.
//!
//! Everything here is best effort: a problem is reported as a build warning,
//! never as a build failure.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

const LIBRARY: &str = "DirectML.dll";

/// Directories under `target/<profile>/` that hold binaries of their own.
const BINARY_DIRECTORIES: [&str; 2] = ["deps", "examples"];

/// `target/<profile>/`, found from `OUT_DIR`, which Cargo sets to
/// `target/<profile>/build/<package>-<hash>/out`.
fn profile_directory() -> Option<PathBuf> {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR")?);
    out_dir.ancestors().nth(3).map(Path::to_path_buf)
}

fn copy_into(source: &Path, directory: &Path) -> io::Result<()> {
    let target = directory.join(LIBRARY);
    let up_to_date = fs::metadata(&target)
        .and_then(|target| Ok(target.len() == fs::metadata(source)?.len()))
        .unwrap_or(false);
    if up_to_date {
        return Ok(());
    }
    fs::create_dir_all(directory)?;
    fs::copy(source, &target)?;
    Ok(())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The target platform, not the one the build script itself runs on.
    if env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    let Some(profile) = profile_directory() else {
        println!("cargo:warning=cannot locate the target directory; {LIBRARY} was not copied");
        return;
    };
    let source = profile.join(LIBRARY);
    // Run again once `ort-sys` has put the library in place or replaced it.
    println!("cargo:rerun-if-changed={}", source.display());
    if !source.is_file() {
        return;
    }
    for directory in BINARY_DIRECTORIES {
        let directory = profile.join(directory);
        if let Err(error) = copy_into(&source, &directory) {
            println!(
                "cargo:warning=cannot copy {LIBRARY} to {}: {error}",
                directory.display()
            );
        }
    }
}
