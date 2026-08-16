//! The web panel's static bundle, embedded into the daemon binary.
//!
//! The daemon is otherwise a pure WebSocket endpoint; serving the panel from
//! the same port is what makes "start daemon, open browser" the whole setup
//! story (and what lets the panel share the WS port's auth/transport rules).
//! `build.rs` codegens the file table from `apps/web/dist`; when the dist was
//! never built a placeholder page is embedded so compilation never requires
//! the Node toolchain.

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// One embedded file.
pub struct Asset {
    /// Content-Type header value.
    pub content_type: &'static str,
    pub bytes: &'static [u8],
    /// Hashed-filename bundle asset (`/assets/…`): safe to cache forever.
    pub immutable: bool,
}

/// Whether a real `apps/web/dist` was embedded (vs. the placeholder page).
pub fn real_dist_embedded() -> bool {
    REAL_DIST
}

/// Look up an embedded asset by URL path. `/` and directory paths resolve to
/// `index.html`; unknown paths fall back to `index.html` as well (the panel
/// is a single-page app whose only state lives in the query string).
///
/// Returns `None` only for paths that are not ours to answer (currently
/// none) — the SPA fallback means every GET path serves something.
pub fn lookup(url_path: &str) -> Asset {
    let path = url_path.split(['?', '#']).next().unwrap_or("/");
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    for (file, content_type, bytes) in FILES {
        if *file == path {
            return Asset {
                content_type,
                bytes,
                immutable: path.starts_with("assets/"),
            };
        }
    }
    // SPA fallback.
    for (file, content_type, bytes) in FILES {
        if *file == "index.html" {
            return Asset {
                content_type,
                bytes,
                immutable: false,
            };
        }
    }
    unreachable!("build.rs always emits at least index.html");
}

#[cfg(test)]
mod tests {
    #[test]
    fn index_is_served_for_root_and_unknown_paths() {
        assert_eq!(super::lookup("/").content_type, "text/html; charset=utf-8");
        assert_eq!(
            super::lookup("/no/such/route").content_type,
            "text/html; charset=utf-8"
        );
    }

    #[test]
    fn query_strings_are_stripped() {
        let asset = super::lookup("/?ws=ws%3A%2F%2F127.0.0.1%3A9800");
        assert_eq!(asset.content_type, "text/html; charset=utf-8");
    }
}
