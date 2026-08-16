//! Build script: pins ts-rs's large-integer mapping for the web wire-type
//! export (`apps/web/src/lib/generated/wire.gen.ts`).
//!
//! ts-rs renders `u64`/`i64` as TypeScript `bigint` by default, but the wire
//! protocol is JSON, where every integer is a plain number. The generated
//! `#[ts(export)]` tests build their config via `ts_rs::Config::from_env()`,
//! which reads `TS_RS_LARGE_INT` at test runtime; cargo propagates
//! `rustc-env` variables to the package's own test binaries, so
//! `cargo test -p neenee-contracts` regenerates the bindings with `number`
//! and no caller-side setup.
fn main() {
    println!("cargo::rustc-env=TS_RS_LARGE_INT=number");
}
