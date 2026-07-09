# neenee-editor

A small code editor rendered through [optics](https://github.com/ming2k/optics),
architected after [Zed](https://github.com/zed-industries/zed) but implemented
from scratch in Rust.

iris owns the Wayland window and event loop; lens draws the chrome (filename,
status bar); flux + flux-text paint the document surface (gutter, wrapped text,
selections, blinking caret) inside iris's paint callback, under the chrome.

## Layout

| module      | role                                                              |
|-------------|-------------------------------------------------------------------|
| `buffer`    | line-aware UTF-8 gap buffer + `Offset`/`Point` + grapheme walks  |
| `selection` | `Selection` (head + anchor) + multi-cursor `Selections`          |
| `history`   | transaction undo/redo with edit coalescing                       |
| `display`   | buffer → wrapped visual lines (DisplayMap-lite, via flux-text-layout) |
| `editor`    | the controller: movement / editing commands → edits              |
| `render`    | flux/flux-text document painting inside iris's paint callback    |
| `main.rs`   | application shell: iris window, lens chrome, keymap, open/save   |

`buffer`/`selection`/`history`/`editor` are pure Rust — no optics dependency —
so the crate builds and tests headless with `--no-default-features`. Only
`display`/`render`/`main.rs` (the `gui` feature) touch optics.

## Build

optics must be built first (the Rust bindings link its C libraries via
pkg-config, resolving the sibling `../optics/build` meson tree):

```bash
cd ../optics && meson setup build -Dexamples=false -Dtests=false && meson compile -C build
cd ../neenee && cargo build -p neenee-editor
```

The crate's `build.rs` bakes an rpath to the optics shared libraries into the
binary, so it runs without `LD_LIBRARY_PATH`. (On a system `meson install` the
pkg-config probe finds the install prefix and rpaths there.)

## Run

```bash
cargo run -p neenee-editor -- path/to/file.txt
```

## Key bindings

| key | action |
|-----|--------|
| arrows | move caret (shift = select) |
| Ctrl-← / Ctrl-→ | move by word |
| Home / End | line start/end (Ctrl = doc start/end) |
| Backspace / Delete | delete prev / next grapheme |
| Enter / Tab | insert newline / 4 spaces |
| Ctrl-S | save · Ctrl-O | open · Ctrl-Z | undo · Ctrl-Y | redo · Ctrl-A | select all |

## Test

The headless core has unit + integration tests (no GPU/compositor needed):

```bash
cargo test -p neenee-editor --no-default-features
```
