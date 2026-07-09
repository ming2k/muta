//! Build script for `neenee-editor`.
//!
//! The optics C libraries (libflux / libflux_text / liblens / libiris) live in
//! the meson build tree at `../optics/build/libs/...` during development. The
//! `-sys` crates locate them via pkg-config and add the right
//! `rustc-link-search` lines, but `rustc-link-arg` (the rpath flag) does not
//! propagate across crates — so a downstream binary ends up linked but unable
//! to find the `.so`s at runtime without `LD_LIBRARY_PATH`.
//!
//! This script re-discovers the optics link directories the same way
//! `iris-sys`/`flux-sys` do (pkg-config, falling back to the sibling meson
//! build tree) and emits the rpaths for *this* binary directly. Mirrors the
//! relay that the `iris` crate's own build.rs does for its examples/tests.
//!
//! On a clean system install (the `.pc` files in `/usr/lib/...`) the probe
//! finds the system dirs and the rpaths point there harmlessly.

use std::path::PathBuf;

fn main() {
    for dir in discover_optics_link_dirs() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
    }
    // Old-style DT_RPATH (not DT_RUNPATH) so transitive deps of libiris
    // (liblens, libflux) are also resolved from the same dirs.
    println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
}

/// Locate the optics shared-library directories. Tries pkg-config first
/// (matches a system install), then falls back to a sibling `../optics/build`
/// meson tree (the local-dev layout this repo ships with).
fn discover_optics_link_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 1. pkg-config for each optics component.
    for pkg in ["iris", "lens", "flux", "flux-text"] {
        if let Ok(lib) = pkg_config::Config::new()
            .print_system_libs(false)
            .probe(pkg)
        {
            for d in lib.link_paths {
                if !dirs.contains(&d) {
                    dirs.push(d);
                }
            }
        }
    }

    // 2. Sibling meson build tree fallback (local dev). Walk up from this
    //    crate's manifest dir looking for `<ancestor>/optics/build/libs`.
    if let Some(manifest) = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) {
        for ancestor in manifest.ancestors() {
            let libs = ancestor.join("optics/build/libs");
            if libs.join("iris/libiris.so").exists() {
                for sub in ["iris", "lens", "flux", "flux/text"] {
                    let d = libs.join(sub);
                    if d.exists() && !dirs.contains(&d) {
                        dirs.push(d);
                    }
                }
                break;
            }
        }
    }

    dirs
}
