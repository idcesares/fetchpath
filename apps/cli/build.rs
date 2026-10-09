//! Link the Visual C++ runtime statically on Windows MSVC.
//!
//! A clean Windows install has the Universal CRT but not VCRUNTIME140.dll. A
//! `fetchpath.exe` that imports it fails to load there and, run from a
//! terminal, exits without printing anything. The desktop app avoids this
//! through tauri-build; this is the same technique for the CLI, from
//! <https://github.com/ChrisDenton/static_vcruntime/> (MIT OR Apache-2.0),
//! as adapted by tauri-build 2.6.3 (`src/static_vcruntime.rs`). The UCRT stays
//! dynamic: it ships with every supported Windows.

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

fn main() {
    write_web_assets();
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

/// The browser entry the desktop package builds (`pnpm build:web`). It is
/// embedded when present; a plain `cargo build` without it still succeeds and
/// serves the placeholder page.
fn write_web_assets() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let bundle = Path::new(&manifest).join("../desktop/dist-web");
    let mut files = Vec::new();
    // Watched either way, so a bundle that appears later is noticed.
    println!("cargo:rerun-if-changed={}", bundle.display());
    println!("cargo:rerun-if-env-changed=FETCHPATH_REQUIRE_WEB");
    if bundle.join("index.html").is_file() {
        collect(&bundle, &bundle, &mut files);
    } else if env::var_os("FETCHPATH_REQUIRE_WEB").is_some() {
        panic!(
            "FETCHPATH_REQUIRE_WEB is set but apps/desktop/dist-web is not built (pnpm build:web)"
        );
    }
    files.sort();
    let mut table = String::from("&[\n");
    for (url, path, mime) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        table.push_str(&format!(
            "    ({url:?}, include_bytes!({:?}), {mime:?}),\n",
            path.display().to_string()
        ));
    }
    table.push_str("]\n");
    fs::write(Path::new(&out_dir).join("web_assets.rs"), table).expect("write web_assets.rs");
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf, &'static str)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files);
            continue;
        }
        let Some(mime) = mime(&path) else { continue };
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let url = format!("/{}", relative.to_string_lossy().replace('\\', "/"));
        files.push((url, path, mime));
    }
}

fn mime(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" => "application/json",
        _ => return None,
    })
}
