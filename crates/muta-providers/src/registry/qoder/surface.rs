//! Qoder's declarative wire surface: Alibaba Qoder's subscription coding
//! platform (`api*.qoder.sh`, CN: `*.qoder.com.cn`), COSY-signed SSE.
//!
//! Everything the executor needs to speak this surface is here as data — the
//! inference path and its fixed query, the emulated client version, the
//! envelope the body is wrapped in, the slots that carry the model identity,
//! and the live catalog endpoint.

use muta_contracts::wire_surface::{
    AgentChatSpec, CatalogSpec, DialectSurface, Envelope, IdentitySpec, IdentityValue,
    InferenceSpec, ModelBinding, ModelCarrier,
};

/// Qoder's per-request inference path (the URL pathname minus the base host).
pub const INFERENCE_PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation";

/// Qoder's fixed inference query, in wire order.
pub const INFERENCE_QUERY: &[(&str, &str)] = &[
    ("FetchKeys", "llm_model_result"),
    ("AgentId", "agent_common"),
    ("Encode", "1"),
];

/// The path form that participates in the COSY signature: the URL pathname
/// minus the `/algo` prefix, no query string.
pub const SIGNED_PATH: &str = "/api/v2/service/pro/sse/agent_chat_generation";

/// The catalog path (the URL pathname minus the base host).
#[allow(dead_code)]
pub const CATALOG_PATH: &str = "/algo/api/v2/model/list";

/// The catalog's signed-path form (the URL pathname minus the `/algo` prefix,
/// no query). One signer covers both this and [`SIGNED_PATH`].
#[allow(dead_code)]
pub const CATALOG_SIGNED_PATH: &str = "/api/v2/model/list";

/// The COSY protocol version string this surface emulates.
pub const COSY_VERSION: &str = "1.1.58";

/// The static identity headers Qoder's client always sends.
pub const IDENTITY_HEADERS: &[(&str, &str)] = &[
    ("Accept", "text/event-stream"),
    ("Cache-Control", "no-cache"),
    ("Connection", "keep-alive"),
    ("Cosy-ClientType", "5"),
    ("Cosy-MachineType", "5"),
    ("Cosy-Business-Product", "cli"),
    ("Cosy-Business-Type", "agent"),
    ("Cosy-Scene", "assistant"),
    ("Cosy-Data-Policy", "agree"),
    ("Login-Version", "v2"),
];

/// The header that carries [`COSY_VERSION`].
pub const VERSION_HEADER: &str = "Cosy-Version";

/// The `agent_chat_generation` envelope's literal slots.
pub const AGENT_CHAT: AgentChatSpec = AgentChatSpec {
    chat_task: "FREE_INPUT",
    agent_id: "agent_common",
    session_type: "qodercli",
    task_id: "common",
    source: 1,
    version: "3",
    model_format: "openai",
    business_product: "cli",
    business_type: "agent",
    business_stage: "start",
    fresh_uuid_pointers: &["request_id", "request_set_id", "chat_record_id"],
};

/// The slots that carry the model identity.
pub const MODEL_BINDINGS: &[ModelBinding] = &[
    ModelBinding {
        carrier: ModelCarrier::Header("X-Model-Key"),
        value: IdentityValue::WireId,
    },
    ModelBinding {
        carrier: ModelCarrier::Header("X-Model-Source"),
        value: IdentityValue::CatalogSource,
    },
    ModelBinding {
        carrier: ModelCarrier::BodyPointer("model_config/key"),
        value: IdentityValue::WireId,
    },
    ModelBinding {
        carrier: ModelCarrier::BodyPointer("model_config/display_name"),
        value: IdentityValue::DisplayName,
    },
    ModelBinding {
        carrier: ModelCarrier::BodyPointer("model_config/source"),
        value: IdentityValue::CatalogSource,
    },
];

/// The declarative surface for the Qoder subscription dialect.
pub const QODER_SURFACE: DialectSurface = DialectSurface {
    identity: IdentitySpec {
        emulated_version: COSY_VERSION,
        version_header: Some(VERSION_HEADER),
        headers: IDENTITY_HEADERS,
    },
    inference: InferenceSpec {
        path: INFERENCE_PATH,
        query: INFERENCE_QUERY,
        signed_path: Some(SIGNED_PATH),
        envelope: Envelope::AgentChat(&AGENT_CHAT),
        model_bindings: MODEL_BINDINGS,
    },
    catalog: Some(CatalogSpec {
        shape: muta_contracts::provider_surface::CatalogShape::SceneMap,
    }),
};
