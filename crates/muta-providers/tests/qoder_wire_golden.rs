//! Golden-wire integration tests for Qoder's shaped pipeline ([INV-WIRE-01],
//! ADR-0271).
//!
//! These observe the *planned* outbound request exactly as the executor would
//! send it — URL, header set, and body bytes. They exist because the
//! 0.50.5→0.50.7 pipeline refactor left the phases unwired and every
//! phase-level test stayed green while production 404'd. This crate hosts the
//! tests because it is the only one that can reference both the executor
//! (`muta-llm-client`) and the Qoder wire implementation without inverting the
//! dependency graph.

use muta_contracts::{
    ClientPreset, ResolvedAuth, SecretString,
    TransportTelemetry,
};
use muta_llm_client::protocol::openai::chat_completions::OpenAiChatCompletionsProvider;
use muta_providers::qoder::{build_qoder_pipeline, QoderRequestIdentity};

/// A static OAuth-like credential source: token + typed Qoder identity.
#[derive(Debug)]
struct StaticOAuthSource {
    token: &'static str,
    identity: QoderRequestIdentity,
}

impl muta_contracts::CredentialSource for StaticOAuthSource {
    fn resolve_auth(&self) -> futures::future::BoxFuture<'_, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            Ok(ResolvedAuth::new(self.token).with_extension(self.identity.clone()))
        })
    }
    fn force_refresh(&self) -> futures::future::BoxFuture<'_, Result<ResolvedAuth, String>> {
        unimplemented!("golden-wire tests never trigger a refresh")
    }
    fn is_oauth(&self) -> bool {
        true
    }
}

fn qoder_test_identity() -> QoderRequestIdentity {
    QoderRequestIdentity {
        uid: "u_test_uid".to_string(),
        machine_key_hex: SecretString::new("0123456789abcdef0123456789abcdef".to_string()),
        data_policy_agreed: true,
        organization_id: None,
        organization_tags: Vec::new(),
    }
}

fn qoder_wire_provider() -> OpenAiChatCompletionsProvider {
    OpenAiChatCompletionsProvider::with_credentials(
        std::sync::Arc::new(StaticOAuthSource {
            token: "exchange-token-1",
            identity: qoder_test_identity(),
        }),
        "qfmodel".to_string(),
        "https://api2.qoder.sh",
        ClientPreset::Native,
    )
    .with_dialect(muta_contracts::OpenAiChatDialect::Qoder)
    .with_pipeline(build_qoder_pipeline())
}

fn qoder_body() -> serde_json::Value {
    serde_json::json!({
        "model": "qfmodel",
        "messages": [{"role": "user", "content": "Say OK only."}],
        "stream": true,
    })
}

fn planned_request(
    provider: &OpenAiChatCompletionsProvider,
    auth: &ResolvedAuth,
) -> muta_llm_client::request::RequestBuilder {
    provider
        .execute_plan(&qoder_body(), auth, &TransportTelemetry::default())
        .expect("plan")
}

#[test]
fn qoder_golden_wire_url_is_the_surface_inference_url() {
    let auth = ResolvedAuth::new("exchange-token-1").with_extension(qoder_test_identity());
    let req = planned_request(&qoder_wire_provider(), &auth).build("QoderGoldenWire").expect("build");

    // The surface's inference path + fixed query — NOT the bare root (the
    // regression: a flat JSON POST to https://api2.qoder.sh/ → HTTP 404).
    assert_eq!(
        req.url,
        "https://api2.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation\
         ?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1"
    );
}

#[test]
fn qoder_golden_wire_headers_carry_the_cosy_signature_set() {
    let auth = ResolvedAuth::new("exchange-token-1").with_extension(qoder_test_identity());
    let req = planned_request(&qoder_wire_provider(), &auth).build("QoderGoldenWire").expect("build");
    let h = &req.headers;

    let authorization = h.get("authorization").unwrap().to_str().unwrap();
    assert!(
        authorization.starts_with("Bearer COSY."),
        "COSY-signed authorization required, got: {authorization}"
    );
    assert!(h.get("cosy-date").is_some(), "Cosy-Date header required");
    assert!(h.get("cosy-key").is_some(), "Cosy-Key header required");
    assert_eq!(h.get("cosy-user").unwrap(), "u_test_uid");
    assert_eq!(h.get("x-model-key").unwrap(), "qfmodel");
    assert_eq!(h.get("x-model-source").unwrap(), "system");
    assert_eq!(h.get("cosy-version").unwrap(), "1.1.58");
    assert_eq!(h.get("cosy-business-product").unwrap(), "cli");
    assert_eq!(h.get("cosy-business-type").unwrap(), "agent");
    assert!(h.get("content-type").is_some());
    // No plain bearer leaks alongside the COSY signature.
    assert!(!authorization.contains("exchange-token-1"));
}

#[test]
fn qoder_golden_wire_body_is_the_qoderencoded_agent_chat_envelope() {
    let auth = ResolvedAuth::new("exchange-token-1").with_extension(qoder_test_identity());
    let req = planned_request(&qoder_wire_provider(), &auth)
        .build("QoderGoldenWire")
        .expect("build");

    let body = req.body.expect("planned body");
    let body_text = std::str::from_utf8(&body).unwrap();
    assert!(
        !body_text.trim_start().starts_with('{'),
        "body must be QoderEncoding-encoded, not flat JSON: {body_text}"
    );

    // Round-trip through the codec, then assert the envelope structurally.
    let decoded = muta_providers::qoder::wire_decode(body_text)
        .expect("body decodes through the Qoder codec");
    let envelope: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(envelope["chat_task"], "FREE_INPUT");
    assert_eq!(envelope["agent_id"], "agent_common");
    assert_eq!(envelope["session_type"], "qodercli");
    assert_eq!(envelope["model_config"]["key"], "qfmodel");
    assert_eq!(envelope["model_config"]["display_name"], "qfmodel");
    assert_eq!(envelope["model_config"]["source"], "system");
    assert_eq!(envelope["business"]["product"], "cli");
    assert_eq!(envelope["business"]["version"], "1.1.58");
    assert_eq!(envelope["messages"][0]["content"], "Say OK only.");
    for slot in ["request_id", "request_set_id", "chat_record_id"] {
        assert!(
            !envelope[slot].as_str().unwrap_or_default().is_empty(),
            "fresh-UUID slot `{slot}` must be populated"
        );
    }
}

#[test]
fn standard_dialect_golden_wire_is_the_plain_chat_completions_wire() {
    let provider = OpenAiChatCompletionsProvider::with_base_url(
        "test-key".to_string(),
        "glm-5.2".to_string(),
        "https://api.example.com/v1/chat/completions",
    );
    let auth = ResolvedAuth::new("test-key");
    let body = serde_json::json!({"model": "glm-5.2", "messages": [], "stream": true});

    let req = provider
        .execute_plan(&body, &auth, &TransportTelemetry::default())
        .expect("plan")
        .build("StandardGoldenWire")
        .expect("build");

    // Pass-through plan: URL unchanged, flat JSON body, bearer header.
    assert_eq!(req.url, "https://api.example.com/v1/chat/completions");
    assert_eq!(req.headers.get("authorization").unwrap(), "Bearer test-key");
    let body_bytes = req.body.expect("body");
    let body_text = std::str::from_utf8(&body_bytes).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(body_text).unwrap();
    assert_eq!(parsed["model"], "glm-5.2");
}
