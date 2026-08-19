//! The local-loopback OAuth callback server for desktop browser authorization.
//!
//! Features:
//! - Flexible port binding strategies: Fixed port, Dynamic OS port (0), or Preferred-with-Dynamic-fallback.
//! - Anti-CSRF PKCE `state` validation.
//! - Modern HTML success & error response pages.
//! - Manual code/URL injection support for seamless CLI paste integration.

use crate::oauth::config::{OAuthConfig, PortMode};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// The outcome of an authorization attempt.
#[derive(Debug, Clone)]
pub enum CallbackOutcome {
    /// The authorization `code`, ready to exchange for tokens.
    Code(String),
    /// The user denied, or the request was malformed / a CSRF mismatch.
    Failed(String),
}

/// The single in-flight callback we are waiting for.
struct Pending {
    state: String,
    tx: oneshot::Sender<CallbackOutcome>,
}

/// Owns the loopback listener, the actual bound port, and its pending-callback slot.
pub struct CallbackServer {
    bound_port: u16,
    pending: Arc<Mutex<Option<Pending>>>,
    _handle: tokio::task::JoinHandle<()>,
}

impl CallbackServer {
    /// Bind the loopback server according to the provider's [`PortMode`].
    pub async fn start_for(cfg: &OAuthConfig) -> Result<Self, std::io::Error> {
        let host = &cfg.oauth_host;
        let path = cfg.oauth_path.to_string();
        let label = cfg.provider_id.to_string();
        let pending = Arc::new(Mutex::new(None::<Pending>));
        let pending_for_task = Arc::clone(&pending);

        let listener = match cfg.port_mode {
            PortMode::Fixed(port) => tokio::net::TcpListener::bind((host.as_ref(), port)).await?,
            PortMode::Dynamic => tokio::net::TcpListener::bind((host.as_ref(), 0)).await?,
            PortMode::PreferredOrDynamic(preferred) => {
                match tokio::net::TcpListener::bind((host.as_ref(), preferred)).await {
                    Ok(l) => l,
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                        tracing::info!(
                            preferred_port = preferred,
                            provider = %label,
                            "preferred port is busy, falling back to dynamic port"
                        );
                        tokio::net::TcpListener::bind((host.as_ref(), 0)).await?
                    }
                    Err(e) => return Err(e),
                }
            }
        };

        let bound_port = listener.local_addr()?.port();
        tracing::debug!(
            provider = %label,
            bound_port = bound_port,
            "OAuth callback server listening"
        );

        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!(error = %e, "{label} oauth callback accept failed");
                        continue;
                    }
                };
                let pending = Arc::clone(&pending_for_task);
                let path = path.clone();
                let label = label.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_one(stream, pending, &path, &label).await {
                        tracing::warn!(error = %e, "{label} oauth callback serve failed");
                    }
                });
            }
        });

        Ok(Self {
            bound_port,
            pending,
            _handle: handle,
        })
    }

    /// The actual port the loopback server bound to.
    pub fn bound_port(&self) -> u16 {
        self.bound_port
    }

    /// Register an in-flight callback expectation for `state` and return a
    /// receiver that resolves with the outcome. Supersedes any prior pending
    /// callback.
    pub fn wait_for_code(&self, state: String) -> oneshot::Receiver<CallbackOutcome> {
        let (tx, rx) = oneshot::channel();
        let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prior) = guard.take() {
            let _ = prior.tx.send(CallbackOutcome::Failed(
                "superseded by a newer authorization request".to_string(),
            ));
        }
        *guard = Some(Pending { state, tx });
        rx
    }

    /// Manually inject an authorization code or failure (e.g. from terminal paste).
    pub fn inject_outcome(&self, outcome: CallbackOutcome) -> bool {
        let mut guard = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = guard.take() {
            let _ = p.tx.send(outcome);
            true
        } else {
            false
        }
    }
}

impl Drop for CallbackServer {
    fn drop(&mut self) {
        self._handle.abort();
    }
}

/// Serve a single callback request. Parses `code`/`state`/`error`, resolves
/// the pending callback if the state matches, and replies with a clean HTML page.
async fn serve_one(
    mut stream: tokio::net::TcpStream,
    pending: Arc<Mutex<Option<Pending>>>,
    redirect_path: &str,
    label: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    let request_line = request.lines().next().unwrap_or("");
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let (pathname, query) = path.split_once('?').unwrap_or((path, ""));

    let params = parse_query(query);
    let code = params.get("code").cloned();
    let state = params.get("state").cloned();
    let error = params.get("error").cloned();
    let error_description = params.get("error_description").cloned();

    let body = {
        let mut guard = pending.lock().unwrap_or_else(|e| e.into_inner());
        let (outcome, body) = match (error.as_deref(), code.as_deref(), state.as_deref()) {
            (Some(err), _, _) => {
                let msg = error_description.unwrap_or_else(|| err.to_string());
                (
                    Some(CallbackOutcome::Failed(msg.clone())),
                    render_page(label, &msg, false),
                )
            }
            (_, None, _) => {
                let msg = "missing authorization code";
                (
                    Some(CallbackOutcome::Failed(msg.to_string())),
                    render_page(label, msg, false),
                )
            }
            (_, Some(c), Some(s)) => {
                if let Some(p) = guard.as_ref()
                    && p.state == s
                {
                    (
                        Some(CallbackOutcome::Code(c.to_string())),
                        render_page(
                            label,
                            "Authorization successful! You may now close this tab and return to the terminal.",
                            true,
                        ),
                    )
                } else {
                    (
                        Some(CallbackOutcome::Failed(
                            "invalid state - potential CSRF mismatch".to_string(),
                        )),
                        render_page(
                            label,
                            "Security check failed: invalid authorization state.",
                            false,
                        ),
                    )
                }
            }
            _ => (
                None,
                render_page(
                    label,
                    "Invalid or unrecognized authorization request.",
                    false,
                ),
            ),
        };
        if let Some(p) = guard.take()
            && let Some(outcome) = outcome
        {
            let _ = p.tx.send(outcome);
        }
        body
    };

    let response = if pathname == redirect_path || redirect_path.ends_with(pathname) {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
    } else if pathname == "/cancel" {
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nLogin cancelled"
            .to_string()
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

fn render_page(label: &str, message: &str, ok: bool) -> String {
    let title = if ok {
        format!("{label} • Authorization Successful")
    } else {
        format!("{label} • Authorization Failed")
    };
    let accent_color = if ok { "#10B981" } else { "#EF4444" };
    let icon = if ok { "✓" } else { "✕" };

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{title}</title>
  <style>
    :root {{
      --bg: #090A0F;
      --card: #131722;
      --text: #F3F4F6;
      --muted: #9CA3AF;
      --accent: {accent_color};
    }}
    @media (prefers-color-scheme: light) {{
      :root {{
        --bg: #F8FAFC;
        --card: #FFFFFF;
        --text: #0F172A;
        --muted: #64748B;
        --accent: {accent_color};
      }}
    }}
    body {{
      margin: 0;
      min-height: 100vh;
      display: flex;
      align-items: center;
      justify-content: center;
      background: var(--bg);
      color: var(--text);
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    }}
    .card {{
      background: var(--card);
      border-radius: 16px;
      padding: 40px;
      max-width: 460px;
      margin: 20px;
      box-shadow: 0 20px 40px rgba(0,0,0,0.2);
      text-align: center;
      border: 1px solid rgba(255,255,255,0.08);
    }}
    .icon {{
      width: 56px;
      height: 56px;
      border-radius: 50%;
      background: var(--accent);
      color: #FFF;
      font-size: 28px;
      font-weight: bold;
      display: flex;
      align-items: center;
      justify-content: center;
      margin: 0 auto 20px;
    }}
    h1 {{
      font-size: 22px;
      margin: 0 0 12px;
      font-weight: 600;
    }}
    p {{
      color: var(--muted);
      font-size: 15px;
      line-height: 1.5;
      margin: 0;
    }}
  </style>
</head>
<body>
  <div class="card">
    <div class="icon">{icon}</div>
    <h1>{title}</h1>
    <p>{message}</p>
  </div>
</body>
</html>"#
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

    #[tokio::test]
    async fn callback_server_drops_and_releases_port() {
        let cfg = OAuthConfig::builder("test")
            .oauth_host("127.0.0.1")
            .oauth_port(59923)
            .port_mode(PortMode::Fixed(59923))
            .oauth_path("/callback")
            .build();

        let server1 = CallbackServer::start_for(&cfg).await.expect("bind 1");
        let server2_err = CallbackServer::start_for(&cfg).await;
        assert!(
            server2_err.is_err(),
            "must fail while first server is alive"
        );

        drop(server1);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let server3 = CallbackServer::start_for(&cfg)
            .await
            .expect("bind after drop");
        drop(server3);
    }

    #[tokio::test]
    async fn dynamic_port_binding_succeeds() {
        let cfg = OAuthConfig::builder("test_dynamic")
            .oauth_host("127.0.0.1")
            .port_mode(PortMode::Dynamic)
            .build();

        let server = CallbackServer::start_for(&cfg).await.expect("dynamic bind");
        assert!(server.bound_port() > 0);
        drop(server);
    }
}
