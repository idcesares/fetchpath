//! Link the Visual C++ runtime statically on Windows MSVC.
//!
//! A clean Windows install has the Universal CRT but not VCRUNTIME140.dll. A
//! `fetchpath.exe` that imports it fails to load there and, run from a
//! terminal, exits without printing anything. The desktop app avoids this
//! through tauri-build; this is the same technique for the CLI, from
//! <https://github.com/ChrisDenton/static_vcruntime/> (MIT OR Apache-2.0),
//! as adapted by tauri-build 2.6.3 (`src/static_vcruntime.rs`). The UCRT stays
//! dynamic: it ships with every supported Windows.

use std::{env, fs, io::Write, path::Path};

fn main() {
    let target = env::var("TARGET").unwrap_or_default();
    if !target.ends_with("-pc-windows-msvc") {
        return;
    }
    override_msvcrt_lib();

    // Disable conflicting libraries that aren't hard coded by Rust.
    for lib in [
        "libvcruntimed.lib",
        "vcruntime.lib",
        "vcruntimed.lib",
        "libcmtd.lib",
        "msvcrt.lib",
        "msvcrtd.lib",
        "libucrt.lib",
        "libucrtd.lib",
    ] {
        println!("cargo:rustc-link-arg=/NODEFAULTLIB:{lib}");
    }
    // Static C runtime and vcruntime, dynamic UCRT.
    for lib in ["libcmt.lib", "libvcruntime.lib", "ucrt.lib"] {
        println!("cargo:rustc-link-arg=/DEFAULTLIB:{lib}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}

/// Replace the msvcrt.lib that rustc names on the command line with an
/// (almost) empty import library, so the static runtime above is used.
fn override_msvcrt_lib() {
    let machine: &[u8] = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => &[0x64, 0x86],
        Ok("x86") => &[0x4C, 0x01],
        _ => return,
    };
    let bytes: &[u8] = &[
        1, 0, 94, 3, 96, 98, 60, 0, 0, 0, 1, 0, 0, 0, 0, 0, 132, 1, 46, 100, 114, 101, 99, 116,
        118, 101, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 10, 16, 0, 46, 100, 114, 101, 99, 116, 118, 101, 0, 0, 0, 0, 1, 0, 0, 0, 3, 0, 4, 0,
        0, 0,
    ];
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR is set for build scripts");
    let path = Path::new(&out_dir).join("msvcrt.lib");
    if let Ok(mut f) = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        f.write_all(machine).expect("write msvcrt.lib");
        f.write_all(bytes).expect("write msvcrt.lib");
    }
    println!("cargo:rustc-link-search=native={out_dir}");
}
