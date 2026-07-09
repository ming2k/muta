//! The local-loopback OAuth callback server for the desktop browser flow.
//!
//! xAI rejects `redirect_uri`s that don't match the Grok-CLI client's
//! registration, whose host:port pair is fixed at `127.0.0.1:56121`, so the
//! server MUST bind exactly there. We accept only the registered callback path,
//! validate PKCE `state` against the in-flight request, and surface a simple
//! success/error HTML page.

use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

/// The pinned loopback host/port (part of the Grok-CLI client registration).
pub const OAUTH_HOST: &str = "127.0.0.1";
pub const OAUTH_PORT: u16 = 56121;
pub const OAUTH_REDIRECT_PATH: &str = "/callback";

/// The exact `redirect_uri` registered with the Grok-CLI client.
pub fn redirect_uri() -> String {
    format!("http://{OAUTH_HOST}:{OAUTH_PORT}{OAUTH_REDIRECT_PATH}")
}

/// The result the callback server resolves (or rejects with) once xAI
/// redirects back.
pub enum CallbackOutcome {
    /// The authorization `code`, ready to exchange for tokens.
    Code(String),
    /// The user denied, or the request was malformed / a CSRF mismatch.
    Failed(String),
}

/// The single in-flight callback we are waiting for. Only one authorize flow
/// runs at a time; a new [`start`] supersedes any prior pending one.
struct Pending {
    state: String,
    tx: oneshot::Sender<CallbackOutcome>,
}

/// Owns the loopback listener and its single pending-callback slot.
pub struct CallbackServer {
    pending: Arc<Mutex<Option<Pending>>>,
    _handle: tokio::task::JoinHandle<()>,
}

impl CallbackServer {
    /// Bind `127.0.0.1:56121` and start accepting. Returns a server whose
    /// [`wait_for_code`] resolves once xAI redirects back with a matching
    /// `state`. Dropping the server stops accepting.
    pub async fn start() -> Result<Self, std::io::Error> {
        let pending = Arc::new(Mutex::new(None::<Pending>));
        let pending_for_task = Arc::clone(&pending);

        let listener = tokio::net::TcpListener::bind((OAUTH_HOST, OAUTH_PORT)).await?;
        let handle = tokio::spawn(async move {
            loop {
                // Accept errors (e.g. EMFILE) must not crash the agent; log and
                // keep serving. A bind error is fatal and returned from start().
                let (stream, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!(error = %e, "xai oauth callback accept failed");
                        continue;
                    }
                };
                let pending = Arc::clone(&pending_for_task);
                tokio::spawn(async move {
                    if let Err(e) = serve_one(stream, pending).await {
                        tracing::warn!(error = %e, "xai oauth callback serve failed");
                    }
                });
            }
        });

        Ok(Self {
            pending,
            _handle: handle,
        })
    }

    /// Register an in-flight callback expectation for `state` and return a
    /// receiver that resolves with the outcome. Supersedes any prior pending
    /// callback (its receiver gets a `Failed`).
    pub fn wait_for_code(&self, state: String) -> oneshot::Receiver<CallbackOutcome> {
        let (tx, rx) = oneshot::channel();
        let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        // Supersede a prior in-flight flow: its receiver gets a rejection.
        if let Some(prior) = guard.take()
            && let _ = prior.tx.send(CallbackOutcome::Failed(
                "superseded by a newer xAI authorize request".to_string(),
            ))
        {
        }
        *guard = Some(Pending { state, tx });
        rx
    }
}

/// Serve a single callback request. Parses `code`/`state`/`error`, resolves
/// the pending callback if the state matches, and replies with a minimal HTML
/// page so the browser shows the user a clear result.
async fn serve_one(
    mut stream: tokio::net::TcpStream,
    pending: Arc<Mutex<Option<Pending>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Parse the request line: "GET /callback?code=..&state=.. HTTP/1.1".
    let request_line = request.lines().next().unwrap_or("");
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let (pathname, query) = path.split_once('?').unwrap_or((path, ""));

    let params = parse_query(query);
    let code = params.get("code").cloned();
    let state = params.get("state").cloned();
    let error = params.get("error").cloned();
    let error_description = params.get("error_description").cloned();

    // Resolve the pending callback (state must match) and craft the page.
    // The mutex guard is NOT held across the `.await` below (Send), so resolve
    // + drop it in a tight scope before touching the stream again.
    let body = {
        let mut guard = pending.lock().unwrap_or_else(|e| e.into_inner());
        let (outcome, body) = match (error.as_deref(), code.as_deref(), state.as_deref()) {
            (Some(err), _, _) => {
                let msg = error_description.unwrap_or_else(|| err.to_string());
                (Some(CallbackOutcome::Failed(msg.clone())), page(&msg, false))
            }
            (_, None, _) => {
                let msg = "missing authorization code";
                (Some(CallbackOutcome::Failed(msg.to_string())), page(msg, false))
            }
            (_, Some(c), Some(s)) => {
                if let Some(p) = guard.as_ref()
                    && p.state == s
                {
                    (
                        Some(CallbackOutcome::Code(c.to_string())),
                        page("Authorization complete. You may close this window.", true),
                    )
                } else {
                    (
                        Some(CallbackOutcome::Failed("invalid state - potential CSRF".to_string())),
                        page("invalid state", false),
                    )
                }
            }
            _ => (None, page("bad request", false)),
        };
        if let Some(p) = guard.take()
            && let Some(outcome) = outcome
        {
            let _ = p.tx.send(outcome);
        }
        body
    };

    let response = if pathname == OAUTH_REDIRECT_PATH {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
    } else if pathname == "/cancel" {
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nLogin cancelled".to_string()
    } else {
        "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\nNot found".to_string()
    };
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(decode(k), decode(v));
        }
    }
    map
}

fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '+' => out.push(' '),
            '%' => {
                let hi = chars.next();
                let lo = chars.next();
                if let (Some(hi), Some(lo)) = (hi, lo)
                    && let Ok(byte) = u8::from_str_radix(&format!("{hi}{lo}"), 16)
                {
                    out.push(byte as char);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn page(message: &str, ok: bool) -> String {
    let title = if ok { "xAI login" } else { "xAI login failed" };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
         <style>body{{font-family:system-ui,sans-serif;text-align:center;padding:3rem}}</style>\
         </head><body><h2>{message}</h2></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_decodes_percent_and_plus() {
        let q = parse_query("code=abc&state=ST&error_description=bad+request%3A+denied");
        assert_eq!(q.get("code").map(String::as_str), Some("abc"));
        assert_eq!(q.get("state").map(String::as_str), Some("ST"));
        assert_eq!(
            q.get("error_description").map(String::as_str),
            Some("bad request: denied")
        );
    }

    #[test]
    fn redirect_uri_is_the_pinned_port() {
        assert_eq!(redirect_uri(), "http://127.0.0.1:56121/callback");
    }
}
