use futures::{SinkExt, StreamExt};
use neenee_core::{AgentRequest, AgentResponse, SessionOverview};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request};
use tokio_tungstenite::tungstenite::http::StatusCode;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachAction {
    New,
    Attach(Option<String>),
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum Wire {
    Select {
        action: AttachAction,
    },
    Welcome {
        session_id: String,
        round_counter: u64,
        messages: Vec<neenee_core::Message>,
    },
    Pick {
        sessions: Vec<SessionOverview>,
    },
    Error {
        message: String,
    },
    Request {
        #[serde(flatten)]
        request: AgentRequest,
    },
    Response {
        #[serde(flatten)]
        response: AgentResponse,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeExpose {
    Local,
    Public,
}

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub port: u16,
    pub expose: ServeExpose,
    pub token: Option<String>,
}
impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            port: 0,
            expose: ServeExpose::Local,
            token: None,
        }
    }
}

pub struct ServeHandle {
    pub port: tokio::sync::oneshot::Receiver<u16>,
    pub cancel: tokio_util::sync::CancellationToken,
    pub token: Option<String>,
}

pub fn start_server(
    opts: ServeOptions,
    registry: Arc<crate::registry::SessionRegistry>,
) -> ServeHandle {
    let (actual_port_tx, actual_port_rx) = tokio::sync::oneshot::channel::<u16>();
    let cancel = tokio_util::sync::CancellationToken::new();
    let cc = cancel.clone();
    let token = match (opts.expose, opts.token.clone()) {
        (ServeExpose::Public, None) => Some(generate_token()),
        (_, t) => t,
    };
    let bind_addr: SocketAddr = match opts.expose {
        ServeExpose::Local => ([127, 0, 0, 1], opts.port).into(),
        ServeExpose::Public => ([0, 0, 0, 0], opts.port).into(),
    };
    let tf = token.clone();
    tokio::spawn(async move {
        let listener = match TcpListener::bind(bind_addr).await {
            Ok(l) => {
                let actual = l.local_addr().map(|a| a.port()).unwrap_or(opts.port);
                let _ = actual_port_tx.send(actual);
                tracing::info!(%bind_addr,actual_port=actual,auth=tf.is_some(),"neenee serve: listener started");
                l
            }
            Err(e) => {
                tracing::error!(%bind_addr,error=%e,"neenee serve: failed to bind");
                return;
            }
        };
        loop {
            tokio::select! {_=cc.cancelled()=>{tracing::info!("neenee serve: cancelled");break;}
            ac=listener.accept()=>{let(stream,peer)=match ac{Ok(c)=>c,Err(e)=>{tracing::warn!(error=%e,"neenee serve: accept failed");continue;}};
            let registry=registry.clone();let token=tf.clone();
            tokio::spawn(async move{if let Err(e)=handle_connection(stream,registry,token).await{tracing::warn!(%peer,error=%e,"neenee serve: connection ended");}});}}
        }
    });
    ServeHandle {
        port: actual_port_rx,
        cancel,
        token,
    }
}

fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let h1 = (nanos ^ pid.wrapping_mul(0x9e3779b97f4a7c15)) as u64;
    let h2 = (nanos >> 64 ^ pid.wrapping_mul(0xbf58476d1ce4e5b9)) as u64;
    format!("{h1:016x}{h2:016x}")
}

#[allow(clippy::result_large_err)]
async fn handle_connection(
    stream: tokio::net::TcpStream,
    registry: Arc<crate::registry::SessionRegistry>,
    token: Option<String>,
) -> Result<(), String> {
    let ws_stream = if let Some(expected) = token.as_deref() {
        let expected = expected.to_string();
        tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, resp| {
            if check_bearer(req, &expected) {
                Ok(resp)
            } else {
                reject_unauthorized()
            }
        })
        .await
        .map_err(|e| format!("ws handshake (auth): {e}"))?
    } else {
        tokio_tungstenite::accept_async(stream)
            .await
            .map_err(|e| format!("ws handshake: {e}"))?
    };
    let (mut ws_sink, mut ws_source) = ws_stream.split();
    let action = loop {
        match ws_source.next().await {
            Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                Ok(Wire::Select { action }) => break action,
                Ok(_) => {
                    send_error(&mut ws_sink, "expected Select as the first frame").await?;
                    return Ok(());
                }
                Err(error) => {
                    send_error(&mut ws_sink, &format!("bad first frame: {error}")).await?;
                    return Ok(());
                }
            },
            Some(Ok(_)) => continue,
            Some(Err(error)) => return Err(format!("ws recv before select: {error}")),
            None => return Ok(()),
        }
    };
    let bound = match registry.resolve(action).await {
        crate::registry::ResolveOutcome::Welcome(s) => s,
        crate::registry::ResolveOutcome::Pick { sessions } => {
            let text = serde_json::to_string(&Wire::Pick { sessions })
                .map_err(|e| format!("serialize pick: {e}"))?;
            ws_sink
                .send(WsMessage::Text(text.into()))
                .await
                .map_err(|e| format!("send pick: {e}"))?;
            return Ok(());
        }
        crate::registry::ResolveOutcome::Error(message) => {
            send_error(&mut ws_sink, &message).await?;
            return Ok(());
        }
    };
    let messages = bound.session.full_transcript().await;
    let round_counter = bound.session.round_counter().await;
    let session_id = bound.session.id().await;
    let welcome = serde_json::to_string(&Wire::Welcome {
        session_id,
        round_counter,
        messages,
    })
    .map_err(|e| format!("serialize welcome: {e}"))?;
    ws_sink
        .send(WsMessage::Text(welcome.into()))
        .await
        .map_err(|e| format!("send welcome: {e}"))?;
    let req_tx = bound.req_tx.clone();
    let mut rx = bound.events.subscribe();
    loop {
        tokio::select! {resp=rx.recv()=>{match resp{Ok(resp)=>{let text=serde_json::to_string(&Wire::Response{response:resp}).map_err(|e|format!("serialize response: {e}"))?;if let Err(e)=ws_sink.send(WsMessage::Text(text.into())).await{return Err(format!("ws send: {e}"));}},Err(broadcast::error::RecvError::Lagged(n))=>{tracing::warn!(skipped=n,"neenee serve: client lagged");continue;},Err(broadcast::error::RecvError::Closed)=>break,}},
        msg=ws_source.next()=>{match msg{Some(Ok(WsMessage::Text(text)))=>match serde_json::from_str::<Wire>(&text){Ok(Wire::Request{request})=>{let _=req_tx.send(request);},Ok(_)=>{},Err(e)=>tracing::warn!(error=%e,"neenee serve: bad request json"),},Some(Ok(_))=>{},Some(Err(e))=>return Err(format!("ws recv: {e}")),None=>break,}}}
    }
    Ok(())
}

async fn send_error(
    ws_sink: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        WsMessage,
    >,
    message: &str,
) -> Result<(), String> {
    let text = serde_json::to_string(&Wire::Error {
        message: message.to_string(),
    })
    .map_err(|e| format!("serialize error: {e}"))?;
    ws_sink
        .send(WsMessage::Text(text.into()))
        .await
        .map_err(|e| format!("send error: {e}"))
}

fn check_bearer(req: &Request, expected: &str) -> bool {
    let Some(val) = req.headers().get("Authorization") else {
        return false;
    };
    let Ok(s) = val.to_str() else {
        return false;
    };
    let Some(rest) = s.strip_prefix("Bearer ") else {
        return false;
    };
    rest.trim() == expected
}

#[allow(clippy::result_large_err)]
fn reject_unauthorized() -> Result<tungstenite::handshake::server::Response, ErrorResponse> {
    let body = "Unauthorized".to_string();
    let resp = tungstenite::handshake::server::Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Bearer")
        .body(Some(body))
        .unwrap_or_default();
    Err(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generate_token_is_nonempty_hex() {
        let t = generate_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
    #[test]
    fn attach_action_roundtrips() {
        assert_eq!(
            serde_json::to_string(&AttachAction::New).unwrap(),
            "\"new\""
        );
        assert_eq!(
            serde_json::to_string(&AttachAction::Attach(Some("abc".into()))).unwrap(),
            r#"{"attach":"abc"}"#
        );
        let back: AttachAction = serde_json::from_str(r#"{"attach":"abc"}"#).unwrap();
        assert_eq!(back, AttachAction::Attach(Some("abc".into())));
    }
}
