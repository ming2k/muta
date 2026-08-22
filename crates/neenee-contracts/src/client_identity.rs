//! Client identity presets and customization for LLM connections.
//!
//! Controls the User-Agent and impersonation headers sent by a connection.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Default user agent this project sends to providers.
pub const NEENEE_USER_AGENT: &str = concat!("neenee/", env!("CARGO_PKG_VERSION"));

/// OpenCode version currently impersonated by neenee.
pub const OPENCODE_VERSION: &str = "1.18.18";

/// User-Agent header value sent when impersonating OpenCode.
pub const OPENCODE_USER_AGENT: &str = "opencode/1.18.18";

/// User-Agent header value sent when impersonating Claude Code.
pub const CLAUDE_CODE_USER_AGENT: &str = "claude-code/0.2.29";

/// Client-identity headers used when impersonating Claude Code.
pub const CLAUDE_CODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("x-app", "claude-code"),
    ("anthropic-version", "2023-06-01"),
];

/// User-Agent header value sent when impersonating OpenAI Codex.
pub const CODEX_USER_AGENT: &str = "codex/1.0.0";

/// Client-identity headers used when impersonating OpenAI Codex.
pub const CODEX_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("Openai-Intent", "conversation-edits"),
    ("x-initiator", "user"),
];

/// User-Agent header value sent when impersonating Cline.
pub const CLINE_USER_AGENT: &str = "Cline/3.5.0";

/// Client-identity headers used when impersonating Cline.
pub const CLINE_CLIENT_HEADERS: &[(&str, &str)] =
    &[("X-Title", "Cline"), ("HTTP-Referer", "https://cline.bot")];

/// User-Agent header value sent when impersonating Cursor.
pub const CURSOR_USER_AGENT: &str = "Cursor/0.45.0";

/// Client-identity headers used when impersonating Cursor.
pub const CURSOR_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Cursor"),
    ("x-cursor-client-version", "0.45.0"),
    ("x-ghost-mode", "true"),
];

/// User-Agent header value sent when impersonating Kilo Code.
pub const KILO_CODE_USER_AGENT: &str = "Kilo-Code/5.3.0";

/// Client-identity headers used when impersonating Kilo Code.
pub const KILO_CODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Kilo Code"),
    ("X-Kilocode-Version", "5.3.0"),
    ("HTTP-Referer", "https://kilocode.ai"),
];

/// User-Agent header value sent when impersonating Roo Code.
pub const ROO_CODE_USER_AGENT: &str = "Roo-Code/3.8.0";

/// Client-identity headers used when impersonating Roo Code.
pub const ROO_CODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Roo Code"),
    ("HTTP-Referer", "https://github.com/RooVetGit/Roo-Cline"),
];

/// User-Agent header value sent when impersonating Windsurf.
pub const WINDSURF_USER_AGENT: &str = "Windsurf/1.0.0";

/// Client-identity headers used when impersonating Windsurf.
pub const WINDSURF_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Windsurf"),
    ("HTTP-Referer", "https://codeium.com/windsurf"),
];

/// User-Agent header value sent when impersonating Aider.
pub const AIDER_USER_AGENT: &str = "aider/0.74.0";

/// Client-identity headers used when impersonating Aider.
pub const AIDER_CLIENT_HEADERS: &[(&str, &str)] =
    &[("X-Title", "Aider"), ("HTTP-Referer", "https://aider.chat")];

/// User-Agent header value sent when impersonating Zhipu ZCode.
pub const ZCODE_USER_AGENT: &str = "ZCode/3.5.3";

/// Client-identity headers used when impersonating Zhipu ZCode.
pub const ZCODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Z Code"),
    ("X-ZCode-Agent", "glm"),
];

/// Client-identity headers used for GitHub Copilot Chat requests.
pub const COPILOT_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("Copilot-Integration-Id", "vscode-chat"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
];

/// User-Agent header value sent when impersonating Google Antigravity.
pub const ANTIGRAVITY_USER_AGENT: &str = "antigravity/1.23.2 linux/amd64";

/// Client-identity headers used when impersonating Google Antigravity.
pub const ANTIGRAVITY_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("x-goog-api-client", "gl-go/1.23.2 gdcl/0.1"),
];

/// First-class client identity presets and custom identity for connection impersonation.
#[derive(Debug, Clone, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum ClientIdentity {
    /// Default native identity (`User-Agent: neenee/<version>`).
    #[default]
    Native,
    /// Impersonate OpenCode (`User-Agent: opencode/1.18.18`).
    OpenCode,
    /// Impersonate Claude Code (`User-Agent: claude-code/...`).
    ClaudeCode,
    /// Impersonate OpenAI Codex (`User-Agent: codex/1.0.0`, `Openai-Intent: conversation-edits`).
    Codex,
    /// Impersonate Cline (`User-Agent: Cline/...`, `X-Title: Cline`).
    Cline,
    /// Impersonate Cursor (`User-Agent: Cursor/...`, `x-cursor-client-version: ...`).
    Cursor,
    /// Impersonate Kilo Code (`User-Agent: Kilo-Code/...`).
    KiloCode,
    /// Impersonate Roo Code (`User-Agent: Roo-Code/...`).
    RooCode,
    /// Impersonate Windsurf (`User-Agent: Windsurf/...`).
    Windsurf,
    /// Impersonate Aider (`User-Agent: aider/...`).
    Aider,
    /// Impersonate Zhipu / Z.AI's native ZCode client.
    ZCode,
    /// Impersonate GitHub Copilot / VS Code Chat.
    Copilot,
    /// Impersonate Google Antigravity (`User-Agent: antigravity/...`).
    Antigravity,
    /// Custom client identity with specific User-Agent and optional headers.
    Custom {
        user_agent: String,
        #[ts(type = "Array<[string, string]>")]
        extra_headers: Vec<(String, String)>,
    },
}

impl ClientIdentity {
    /// All standard built-in client identity presets.
    pub const PRESETS: &[ClientIdentity] = &[
        Self::Native,
        Self::OpenCode,
        Self::ClaudeCode,
        Self::Codex,
        Self::Cline,
        Self::Cursor,
        Self::KiloCode,
        Self::RooCode,
        Self::Windsurf,
        Self::Aider,
        Self::ZCode,
        Self::Copilot,
        Self::Antigravity,
    ];

    /// Return the canonical id string for this preset identity.
    pub fn id(&self) -> &str {
        match self {
            Self::Native => "native",
            Self::OpenCode => "opencode",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cline => "cline",
            Self::Cursor => "cursor",
            Self::KiloCode => "kilo-code",
            Self::RooCode => "roo-code",
            Self::Windsurf => "windsurf",
            Self::Aider => "aider",
            Self::ZCode => "zcode",
            Self::Copilot => "copilot",
            Self::Antigravity => "antigravity",
            Self::Custom { .. } => "custom",
        }
    }

    /// Return human-friendly display label.
    pub fn label(&self) -> &str {
        match self {
            Self::Native => "neenee (Native)",
            Self::OpenCode => "OpenCode",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "OpenAI Codex",
            Self::Cline => "Cline",
            Self::Cursor => "Cursor",
            Self::KiloCode => "Kilo Code",
            Self::RooCode => "Roo Code",
            Self::Windsurf => "Windsurf",
            Self::Aider => "Aider",
            Self::ZCode => "ZCode (Z.AI)",
            Self::Copilot => "GitHub Copilot",
            Self::Antigravity => "Antigravity (Google)",
            Self::Custom { .. } => "Custom",
        }
    }

    /// Return the User-Agent header value for this client identity.
    pub fn user_agent(&self) -> &str {
        match self {
            Self::Native => NEENEE_USER_AGENT,
            Self::OpenCode => OPENCODE_USER_AGENT,
            Self::ClaudeCode => CLAUDE_CODE_USER_AGENT,
            Self::Codex => CODEX_USER_AGENT,
            Self::Cline => CLINE_USER_AGENT,
            Self::Cursor => CURSOR_USER_AGENT,
            Self::KiloCode => KILO_CODE_USER_AGENT,
            Self::RooCode => ROO_CODE_USER_AGENT,
            Self::Windsurf => WINDSURF_USER_AGENT,
            Self::Aider => AIDER_USER_AGENT,
            Self::ZCode => ZCODE_USER_AGENT,
            Self::Copilot => NEENEE_USER_AGENT,
            Self::Antigravity => ANTIGRAVITY_USER_AGENT,
            Self::Custom { user_agent, .. } => user_agent.as_str(),
        }
    }

    /// Return the client-identity headers for this client identity.
    pub fn headers(&self) -> Vec<(&str, &str)> {
        match self {
            Self::Native | Self::OpenCode => Vec::new(),
            Self::ClaudeCode => CLAUDE_CODE_CLIENT_HEADERS.to_vec(),
            Self::Codex => CODEX_CLIENT_HEADERS.to_vec(),
            Self::Cline => CLINE_CLIENT_HEADERS.to_vec(),
            Self::Cursor => CURSOR_CLIENT_HEADERS.to_vec(),
            Self::KiloCode => KILO_CODE_CLIENT_HEADERS.to_vec(),
            Self::RooCode => ROO_CODE_CLIENT_HEADERS.to_vec(),
            Self::Windsurf => WINDSURF_CLIENT_HEADERS.to_vec(),
            Self::Aider => AIDER_CLIENT_HEADERS.to_vec(),
            Self::ZCode => ZCODE_CLIENT_HEADERS.to_vec(),
            Self::Copilot => COPILOT_CLIENT_HEADERS.to_vec(),
            Self::Antigravity => ANTIGRAVITY_CLIENT_HEADERS.to_vec(),
            Self::Custom { extra_headers, .. } => extra_headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
        }
    }

    /// Parse a preset from an id or common alias.
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "native" | "neenee" | "default" => Some(Self::Native),
            "opencode" => Some(Self::OpenCode),
            "claude" | "claude-code" | "claudecode" => Some(Self::ClaudeCode),
            "codex" | "openai-codex" => Some(Self::Codex),
            "cline" => Some(Self::Cline),
            "cursor" => Some(Self::Cursor),
            "kilo" | "kilo-code" | "kilocode" => Some(Self::KiloCode),
            "roo" | "roo-code" | "roocode" => Some(Self::RooCode),
            "windsurf" => Some(Self::Windsurf),
            "aider" => Some(Self::Aider),
            "zcode" | "z-code" | "zai" => Some(Self::ZCode),
            "copilot" | "github-copilot" | "vscode" => Some(Self::Copilot),
            "antigravity" | "agy" | "google-antigravity" => Some(Self::Antigravity),
            _ => None,
        }
    }

    /// Resolve a client identity preset from a User-Agent string.
    pub fn from_user_agent(ua: &str) -> Self {
        let trimmed = ua.trim();
        if trimmed.is_empty() || trimmed.starts_with("neenee/") {
            Self::Native
        } else if trimmed.starts_with("opencode") {
            Self::OpenCode
        } else if trimmed.starts_with("claude-code") || trimmed.starts_with("claude-cli") {
            Self::ClaudeCode
        } else if trimmed.starts_with("codex") || trimmed.starts_with("OpenAI-Codex") {
            Self::Codex
        } else if trimmed.starts_with("Cline") || trimmed.starts_with("cline") {
            Self::Cline
        } else if trimmed.starts_with("Cursor") || trimmed.starts_with("cursor") {
            Self::Cursor
        } else if trimmed.starts_with("Kilo-Code") || trimmed.starts_with("kilo") {
            Self::KiloCode
        } else if trimmed.starts_with("Roo-Code") || trimmed.starts_with("roo") {
            Self::RooCode
        } else if trimmed.starts_with("Windsurf") || trimmed.starts_with("windsurf") {
            Self::Windsurf
        } else if trimmed.starts_with("aider") {
            Self::Aider
        } else if trimmed.starts_with("ZCode") || trimmed.starts_with("zcode") {
            Self::ZCode
        } else if trimmed.contains("Copilot") || trimmed.contains("copilot") {
            Self::Copilot
        } else if trimmed.starts_with("antigravity") || trimmed.starts_with("agy") {
            Self::Antigravity
        } else {
            Self::Custom {
                user_agent: trimmed.to_string(),
                extra_headers: Vec::new(),
            }
        }
    }
}

impl Serialize for ClientIdentity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Custom {
                user_agent,
                extra_headers,
            } => {
                use serde::ser::SerializeStruct;
                let mut s = serializer.serialize_struct("ClientIdentity", 2)?;
                s.serialize_field("user_agent", user_agent)?;
                s.serialize_field("extra_headers", extra_headers)?;
                s.end()
            }
            _ => serializer.serialize_str(self.id()),
        }
    }
}

impl<'de> Deserialize<'de> for ClientIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ClientIdentityVisitor;

        impl<'de> serde::de::Visitor<'de> for ClientIdentityVisitor {
            type Value = ClientIdentity;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a client identity preset string or custom identity object")
            }

            fn visit_str<E>(self, value: &str) -> Result<ClientIdentity, E>
            where
                E: serde::de::Error,
            {
                ClientIdentity::from_id(value)
                    .or_else(|| Some(ClientIdentity::from_user_agent(value)))
                    .ok_or_else(|| E::custom(format!("unknown client identity preset: {value}")))
            }

            fn visit_map<M>(self, mut map: M) -> Result<ClientIdentity, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                let mut user_agent = None;
                let mut extra_headers = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "user_agent" => {
                            user_agent = Some(map.next_value()?);
                        }
                        "extra_headers" => {
                            extra_headers = map.next_value()?;
                        }
                        _ => {
                            let _ = map.next_value::<serde_json::Value>()?;
                        }
                    }
                }
                let user_agent = user_agent
                    .ok_or_else(|| serde::de::Error::missing_field("user_agent"))?;
                Ok(ClientIdentity::Custom {
                    user_agent,
                    extra_headers,
                })
            }
        }

        deserializer.deserialize_any(ClientIdentityVisitor)
    }
}
