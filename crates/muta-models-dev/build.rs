//! Emits a `cargo:rerun-if-changed` directive for the checked-in models.dev
//! snapshot so a rebuild is triggered exactly when the snapshot changes.
//!
//! The snapshot itself is fetched and pruned by
//! `scripts/refresh-models-dev-snapshot.sh` and committed to the repo; this
//! build script only embeds it via `include_str!` in `lib.rs`. Builds never
//! touch the network, so they are deterministic and offline-safe.

fn main() {
    // Path is relative to the crate root (CARGO_MANIFEST_DIR).
    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(dir) => dir,
        Err(_) => return, // build.rs always runs under cargo; a missing value
                          // means the harness is broken — skip the directive.
    };
    println!("cargo:rerun-if-changed={manifest_dir}/snapshot.json");
}
