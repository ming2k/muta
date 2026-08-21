// Exercise the build script's source generator as ordinary test code. Keeping
// this as a path module means the production build script and its regression
// tests use exactly the same implementation.
#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;
