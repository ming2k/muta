//! Make locally built optics shared libraries discoverable at runtime.
//!
//! The optics `-sys` crates provide link-search paths while compiling, but an
//! application binary must carry its own runtime search path. System installs
//! are discovered through pkg-config; local development falls back to the
//! sibling `../optics/build/libs` Meson tree.

use std::path::PathBuf;

fn main() {
    if std::env::var_os("CARGO_FEATURE_GUI").is_none() {
        return;
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "linux" | "macos") {
        for dir in discover_optics_link_dirs() {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
        }
    }
    // DT_RPATH also applies while resolving transitive dependencies such as
    // libiris -> liblens -> libflux. DT_RUNPATH would not cover that chain.
    if target_os == "linux" {
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
    }
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
}

fn discover_optics_link_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    for package in ["iris", "lens", "flux", "flux-text"] {
        if let Ok(library) = pkg_config::Config::new()
            .print_system_libs(false)
            .probe(package)
        {
            for path in library.link_paths {
                if !dirs.contains(&path) {
                    dirs.push(path);
                }
            }
        }
    }

    if let Some(manifest_dir) = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) {
        for ancestor in manifest_dir.ancestors() {
            let libs = ancestor.join("optics/build/libs");
            if libs.join("iris/libiris.so").exists() {
                for subdirectory in ["iris", "lens", "flux", "flux/text"] {
                    let path = libs.join(subdirectory);
                    if path.exists() && !dirs.contains(&path) {
                        dirs.push(path);
                    }
                }
                break;
            }
        }
    }

    dirs
}
