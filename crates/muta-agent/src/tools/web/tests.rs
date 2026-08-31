use super::*;
use crate::tools::web::html::extract_html_title;
use muta_contracts::{Tool, WebSearchConfig};
use sha2::{Digest, Sha256};

#[test]
fn html_title_is_normalized_for_watch_summaries() {
    let html = "<html><head><title>  Market &amp; Risk </title></head><body>x</body></html>";
    assert_eq!(extract_html_title(html), "Market & Risk");
    assert_eq!(extract_html_title("<html>untitled</html>"), "");
}

#[test]
fn snapshot_shape_round_trips_through_json() {
    let snapshot = WebPageSnapshot {
        requested_url: "https://example.com/a".to_string(),
        final_url: "https://example.com/a".to_string(),
        title: "A".to_string(),
        text_preview: "preview".to_string(),
        content_hash: format!("{:x}", Sha256::digest(b"body")),
        content_type: "text/html".to_string(),
        etag: Some("v1".to_string()),
        last_modified: None,
        body_bytes: 4,
        checked_at_ms: 1,
    };
    let encoded = serde_json::to_string(&snapshot).expect("snapshot JSON");
    let decoded: WebPageSnapshot = serde_json::from_str(&encoded).expect("snapshot round trip");
    assert_eq!(decoded, snapshot);
}

mod guarded_get_tests {
    use crate::tools::web::client::guarded_get;

    async fn redirect_server(target: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        format!("http://{addr}/hop")
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn redirect_to_metadata_endpoint_is_refused() {
        let url = redirect_server("http://169.254.169.254/latest/meta-data/").await;
        let err = guarded_get(&test_client(), &url, Default::default())
            .await
            .expect_err("redirect into the metadata endpoint must be refused");
        assert!(
            err.contains("SSRF guard"),
            "expected an SSRF-guard error, got: {err}"
        );
    }

    #[tokio::test]
    async fn redirect_to_loopback_is_refused() {
        let url = redirect_server("http://127.0.0.1:9/secret").await;
        let err = guarded_get(&test_client(), &url, Default::default())
            .await
            .expect_err("redirect into loopback must be refused");
        assert!(
            err.contains("SSRF guard"),
            "expected an SSRF-guard error, got: {err}"
        );
    }

    #[tokio::test]
    async fn direct_private_url_is_refused_before_any_connection() {
        let err = guarded_get(&test_client(), "http://10.255.255.1/x", Default::default())
            .await
            .expect_err("private IP must be refused by the pre-flight");
        assert!(err.contains("SSRF guard"));
    }
}

mod shared_config_tests {
    use super::*;
    use muta_contracts::SharedWebSearchConfig;

    #[test]
    fn websearch_chain_rebuilds_when_shared_config_changes() {
        let shared = SharedWebSearchConfig::new(WebSearchConfig::default());
        let tool = WebSearchTool::with_shared_config(shared.clone());
        let (primary, _, _) = tool.current_chain().expect("default chain builds");
        assert_eq!(primary.name(), "Exa");

        shared.set(WebSearchConfig {
            provider: "tavily".to_string(),
            tavily_api_key: Some(muta_contracts::SecretString::new("tvly-x")),
            ..WebSearchConfig::default()
        });
        let (primary, fallback, _) = tool.current_chain().expect("rebuilt chain builds");
        assert_eq!(primary.name(), "Tavily");
        assert_eq!(fallback.expect("default fallback").name(), "Parallel");

        let (again, _, _) = tool.current_chain().expect("cached chain builds");
        assert_eq!(again.name(), "Tavily");
    }

    #[test]
    fn signature_ignores_nothing_that_matters_and_hides_secrets() {
        let mut a = WebSearchConfig::default();
        let b = WebSearchConfig::default();
        assert_eq!(a.signature(), b.signature());
        a.provider = "bocha".to_string();
        assert_ne!(a.signature(), b.signature());
        a.provider = b.provider.clone();
        a.bocha_api_key = Some(muta_contracts::SecretString::new("sk-secret-value"));
        let sig = a.signature();
        assert!(!sig.contains("sk-secret-value"));
        let mut c = a.clone();
        c.bocha_api_key = Some(muta_contracts::SecretString::new("sk-other"));
        assert_ne!(a.signature(), c.signature());
        c.bocha_api_key = None;
        assert_ne!(a.signature(), c.signature());
    }

    #[test]
    fn websearch_and_webfetch_is_available_reflects_configuration() {
        let shared = SharedWebSearchConfig::new(WebSearchConfig::default());
        let search = WebSearchTool::with_shared_config(shared.clone());
        let fetch = WebFetchTool::with_shared_config(shared.clone());

        // Default search (exa) is available, but default reader (jina) without key is not ready.
        assert!(search.is_available());
        assert!(!fetch.is_available());

        // Supplying jina_api_key makes fetch available.
        shared.set(WebSearchConfig {
            jina_api_key: Some(muta_contracts::SecretString::new("jina_xxx")),
            ..WebSearchConfig::default()
        });
        assert!(fetch.is_available());

        shared.set(WebSearchConfig {
            provider: "none".to_string(),
            jina_api_key: Some(muta_contracts::SecretString::new("jina_xxx")),
            ..WebSearchConfig::default()
        });
        assert!(!search.is_available());
        assert!(fetch.is_available());

        shared.set(WebSearchConfig {
            provider: "tavily".to_string(),
            tavily_api_key: None,
            ..WebSearchConfig::default()
        });
        assert!(!search.is_available());
        shared.set(WebSearchConfig {
            provider: "tavily".to_string(),
            tavily_api_key: Some(muta_contracts::SecretString::new("tvly-xxx")),
            ..WebSearchConfig::default()
        });
        assert!(search.is_available());

        shared.set(WebSearchConfig {
            provider: "searxng".to_string(),
            searxng_url: None,
            ..WebSearchConfig::default()
        });
        assert!(!search.is_available());
        shared.set(WebSearchConfig {
            provider: "searxng".to_string(),
            searxng_url: Some("http://localhost:8080".to_string()),
            ..WebSearchConfig::default()
        });
        assert!(search.is_available());

        shared.set(WebSearchConfig {
            reader: "none".to_string(),
            ..WebSearchConfig::default()
        });
        assert!(search.is_available());
        assert!(!fetch.is_available());
    }
}
