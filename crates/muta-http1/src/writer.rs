//! Request serialization.

use bytes::{BufMut, BytesMut};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::error::HttpError;
use crate::head::RequestHead;

/// Serialize `head` plus an optional length-delimited body onto `writer`.
///
/// A `Content-Length` is always written (0 for an absent body) unless the caller
/// already supplied one, so every request is self-delimiting: a proxy or server
/// never has to guess where the body ends.
pub async fn write_request<W: AsyncWrite + Unpin>(
    writer: &mut W,
    head: &RequestHead,
    body: Option<&[u8]>,
) -> Result<(), HttpError> {
    let mut out = BytesMut::with_capacity(512 + body.map_or(0, <[u8]>::len));

    out.put_slice(head.method.as_str().as_bytes());
    out.put_slice(b" ");
    out.put_slice(head.target.as_bytes());
    out.put_slice(b" HTTP/1.1\r\n");

    let mut wrote_length = false;
    for (name, value) in head.headers.iter() {
        if name == http::header::CONTENT_LENGTH {
            wrote_length = true;
        }
        out.put_slice(name.as_str().as_bytes());
        out.put_slice(b": ");
        out.put_slice(value.as_bytes());
        out.put_slice(b"\r\n");
    }
    if !wrote_length {
        let length = body.map_or(0, <[u8]>::len);
        out.put_slice(b"content-length: ");
        out.put_slice(length.to_string().as_bytes());
        out.put_slice(b"\r\n");
    }
    out.put_slice(b"\r\n");
    if let Some(body) = body {
        out.put_slice(body);
    }

    writer.write_all(&out).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::Method;

    async fn encode(head: &RequestHead, body: Option<&[u8]>) -> String {
        let mut out = Vec::new();
        write_request(&mut out, head, body).await.expect("write");
        String::from_utf8(out).expect("utf-8")
    }

    #[tokio::test]
    async fn a_request_is_self_delimiting() {
        let head = RequestHead::new(Method::POST, "/v1/chat/completions")
            .with_header("content-type", "application/json")
            .with_header("authorization", "Bearer secret");
        let text = encode(&head, Some(b"{\"stream\":true}")).await;
        assert!(text.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(text.contains("content-type: application/json\r\n"));
        assert!(text.contains("authorization: Bearer secret\r\n"));
        assert!(text.contains("content-length: 15\r\n"));
        assert!(text.ends_with("\r\n\r\n{\"stream\":true}"));
    }

    #[tokio::test]
    async fn an_absent_body_still_declares_zero_length() {
        let head = RequestHead::new(Method::GET, "/models");
        let text = encode(&head, None).await;
        assert!(text.contains("content-length: 0\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[tokio::test]
    async fn a_caller_supplied_length_is_not_duplicated() {
        let head = RequestHead::new(Method::POST, "/x").with_header("content-length", "3");
        let text = encode(&head, Some(b"abc")).await;
        assert_eq!(text.matches("content-length").count(), 1);
    }
}
