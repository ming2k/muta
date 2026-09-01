//! Client profile presets, specifications, and environment emulation for LLM connections.
//!
//! Provides first-class client identity presets (e.g. OpenCode, Claude Code,
//! Cursor, Antigravity, Copilot, ZCode) and customizable client profiles to ensure
//! accurate wire protocol parity, client compatibility, and feature enablement.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Default user agent this project sends to providers.
pub const MUTA_USER_AGENT: &str = concat!("muta/", env!("CARGO_PKG_VERSION"));

/// OpenCode version emulated by muta.
pub const OPENCODE_VERSION: &str = "1.18.18";

/// User-Agent header value sent for OpenCode client profile.
pub const OPENCODE_USER_AGENT: &str = "opencode/1.18.18";

/// Claude Code version emulated by muta.
pub const CLAUDE_CODE_VERSION: &str = "0.2.29";

/// User-Agent header value sent for Claude Code client profile.
pub const CLAUDE_CODE_USER_AGENT: &str = "claude-code/0.2.29";

/// Client identity headers used for Claude Code profile.
pub const CLAUDE_CODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("x-app", "claude-code"),
    ("anthropic-version", "2023-06-01"),
];

/// OpenAI Codex CLI version emulated by muta.
pub const CODEX_VERSION: &str = "0.151.0";

/// User-Agent header value sent for OpenAI Codex client profile.
pub const CODEX_USER_AGENT: &str = "codex/0.151.0";

/// Client identity headers used for OpenAI Codex profile.
pub const CODEX_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("Openai-Intent", "conversation-edits"),
    ("x-initiator", "user"),
];

/// Cline extension version emulated by muta.
pub const CLINE_VERSION: &str = "3.5.0";

/// User-Agent header value sent for Cline client profile.
pub const CLINE_USER_AGENT: &str = "Cline/3.5.0";

/// Client identity headers used for Cline profile.
pub const CLINE_CLIENT_HEADERS: &[(&str, &str)] =
    &[("X-Title", "Cline"), ("HTTP-Referer", "https://cline.bot")];

/// Cursor version emulated by muta.
pub const CURSOR_VERSION: &str = "0.45.0";

/// User-Agent header value sent for Cursor client profile.
pub const CURSOR_USER_AGENT: &str = "Cursor/0.45.0";

/// Client identity headers used for Cursor profile.
pub const CURSOR_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Cursor"),
    ("x-cursor-client-version", "0.45.0"),
    ("x-ghost-mode", "true"),
];

/// Kilo Code version emulated by muta.
pub const KILO_CODE_VERSION: &str = "5.3.0";

/// User-Agent header value sent for Kilo Code client profile.
pub const KILO_CODE_USER_AGENT: &str = "Kilo-Code/5.3.0";

/// Client identity headers used for Kilo Code profile.
pub const KILO_CODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Kilo Code"),
    ("X-Kilocode-Version", "5.3.0"),
    ("HTTP-Referer", "https://kilocode.ai"),
];

/// Roo Code version emulated by muta.
pub const ROO_CODE_VERSION: &str = "3.8.0";

/// User-Agent header value sent for Roo Code client profile.
pub const ROO_CODE_USER_AGENT: &str = "Roo-Code/3.8.0";

/// Client identity headers used for Roo Code profile.
pub const ROO_CODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Roo Code"),
    ("HTTP-Referer", "https://github.com/RooVetGit/Roo-Cline"),
];

/// Windsurf version emulated by muta.
pub const WINDSURF_VERSION: &str = "1.0.0";

/// User-Agent header value sent for Windsurf client profile.
pub const WINDSURF_USER_AGENT: &str = "Windsurf/1.0.0";

/// Client identity headers used for Windsurf profile.
pub const WINDSURF_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Windsurf"),
    ("HTTP-Referer", "https://codeium.com/windsurf"),
];

/// Aider version emulated by muta.
pub const AIDER_VERSION: &str = "0.74.0";

/// User-Agent header value sent for Aider client profile.
pub const AIDER_USER_AGENT: &str = "aider/0.74.0";

/// Client identity headers used for Aider profile.
pub const AIDER_CLIENT_HEADERS: &[(&str, &str)] =
    &[("X-Title", "Aider"), ("HTTP-Referer", "https://aider.chat")];

/// Zhipu ZCode version emulated by muta.
pub const ZCODE_VERSION: &str = "3.5.3";

/// User-Agent header value sent for Zhipu ZCode client profile.
pub const ZCODE_USER_AGENT: &str = "ZCode/3.5.3";

/// Client identity headers used for Zhipu ZCode profile.
pub const ZCODE_CLIENT_HEADERS: &[(&str, &str)] =
    &[("X-Title", "Z Code"), ("X-ZCode-Agent", "glm")];

/// Client identity headers used for GitHub Copilot Chat requests.
pub const COPILOT_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("Copilot-Integration-Id", "vscode-chat"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
];

/// Google Antigravity application name.
pub const ANTIGRAVITY_APP_NAME: &str = "antigravity";

/// Google Antigravity brand display name (matching `VersionInfo.BrandName`).
pub const ANTIGRAVITY_BRAND_NAME: &str = "Antigravity CLI";

/// Google Jetski fallback brand name.
pub const ANTIGRAVITY_JETSKI_BRAND_NAME: &str = "Jetski CLI";

/// Google Antigravity version emulated by muta.
pub const ANTIGRAVITY_VERSION: &str = "1.23.2";

/// User-Agent header value sent for Google Antigravity client profile.
pub const ANTIGRAVITY_USER_AGENT: &str = "antigravity/1.23.2 linux/amd64";

/// Construct an Antigravity User-Agent string formatted for the target OS and CPU architecture.
pub fn antigravity_user_agent(version: &str, os: &str, arch: &str) -> String {
    format!("antigravity/{version} {os}/{arch}")
}

/// Google API client attribution header value sent by Antigravity CLI.
pub const ANTIGRAVITY_API_CLIENT_HEADER: &str = "gl-go/1.23.2 gdcl/0.1";

/// Remote control proxy identification header: `X-Jetski-Via-Remote-Control`.
pub const ANTIGRAVITY_REMOTE_CONTROL_VIA_HEADER: &str = "X-Jetski-Via-Remote-Control";

/// Remote control proxy user agent header: `X-Jetski-Remote-Control-User-Agent`.
pub const ANTIGRAVITY_REMOTE_CONTROL_UA_HEADER: &str = "X-Jetski-Remote-Control-User-Agent";

/// Remote control proxy transport header: `X-Jetski-Remote-Control-Transport`.
pub const ANTIGRAVITY_REMOTE_CONTROL_TRANSPORT_HEADER: &str = "X-Jetski-Remote-Control-Transport";

/// Client identity headers used for Google Antigravity profile.
pub const ANTIGRAVITY_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("x-goog-api-client", ANTIGRAVITY_API_CLIENT_HEADER),
];

/// Antigravity client environment and session metadata (emulating agy internal `exa.codeium_common_pb.Metadata`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AntigravityClientMetadata {
    /// IDE / CLI application identifier (`"antigravity"` or `"antigravity-cli"`).
    pub ide_name: String,
    /// Semantic version of the host tool.
    pub ide_version: String,
    /// Extension identifier (`"antigravity"`).
    pub extension_name: String,
    /// Extension version string.
    pub extension_version: String,
    /// Canonical product name (`"antigravity"`).
    pub product_name: String,
    /// Operating system name (`"linux"`, `"darwin"`, `"windows"`).
    pub os: String,
    /// Hardware architecture (`"x86_64"`, `"aarch64"`).
    pub arch: String,
    /// Unique session UUIDv4.
    pub session_id: String,
    /// Machine hardware fingerprint / device hash.
    pub device_fingerprint: String,
    /// Runtime environment detection (`"Standalone"`, `"SSH session"`, `"WSL environment"`, `"Container environment"`).
    pub runtime_environment: String,
    /// User tier identifier (`"FREE"`, `"PRO"`, `"ENTERPRISE"`, `"GDP_HELIUM"`).
    pub user_tier_id: Option<String>,
}

impl Default for AntigravityClientMetadata {
    fn default() -> Self {
        Self {
            ide_name: ANTIGRAVITY_APP_NAME.to_string(),
            ide_version: ANTIGRAVITY_VERSION.to_string(),
            extension_name: ANTIGRAVITY_APP_NAME.to_string(),
            extension_version: ANTIGRAVITY_VERSION.to_string(),
            product_name: ANTIGRAVITY_APP_NAME.to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            session_id: String::new(),
            device_fingerprint: String::new(),
            runtime_environment: "Standalone".to_string(),
            user_tier_id: None,
        }
    }
}

impl AntigravityClientMetadata {
    /// Create a new Antigravity client metadata record with session and fingerprint.
    pub fn new(session_id: impl Into<String>, device_fingerprint: impl Into<String>, runtime_env: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            device_fingerprint: device_fingerprint.into(),
            runtime_environment: runtime_env.into(),
            ..Default::default()
        }
    }

    /// Return the canonical User-Agent header value for this metadata.
    pub fn user_agent(&self) -> String {
        antigravity_user_agent(&self.ide_version, &self.os, &self.arch)
    }
}

/// Static metadata specification for a client profile preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientProfileSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub default_version: &'static str,
    pub user_agent: &'static str,
    pub headers: &'static [(&'static str, &'static str)],
    pub capabilities: ClientCapabilities,
}

/// Feature capabilities and protocol traits of a client profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClientCapabilities {
    /// Whether this client profile is recognized by coding-subscription platforms (e.g. Z.AI / Kimi / Copilot).
    pub coding_platform_compatible: bool,
    /// Whether this client profile attaches client-side application attribution headers.
    pub has_client_headers: bool,
}

/// Standard client identity presets supported by muta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum ClientPreset {
    /// Default native identity (`User-Agent: muta/<version>`).
    #[default]
    Native,
    /// OpenCode coding environment.
    OpenCode,
    /// Claude Code CLI.
    ClaudeCode,
    /// OpenAI Codex CLI.
    Codex,
    /// Cline extension.
    Cline,
    /// Cursor editor.
    Cursor,
    /// Kilo Code extension.
    KiloCode,
    /// Roo Code extension.
    RooCode,
    /// Windsurf editor.
    Windsurf,
    /// Aider CLI.
    Aider,
    /// Zhipu ZCode client.
    ZCode,
    /// GitHub Copilot / VS Code Chat.
    Copilot,
    /// Google Antigravity / Cloud Code environment.
    Antigravity,
}

impl ClientPreset {
    /// Return the canonical specification for this preset.
    pub const fn spec(&self) -> ClientProfileSpec {
        match self {
            Self::Native => ClientProfileSpec {
                id: "native",
                label: "muta (Native)",
                default_version: env!("CARGO_PKG_VERSION"),
                user_agent: MUTA_USER_AGENT,
                headers: &[],
                capabilities: ClientCapabilities {
                    coding_platform_compatible: false,
                    has_client_headers: false,
                },
            },
            Self::OpenCode => ClientProfileSpec {
                id: "opencode",
                label: "OpenCode",
                default_version: OPENCODE_VERSION,
                user_agent: OPENCODE_USER_AGENT,
                headers: &[],
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: false,
                },
            },
            Self::ClaudeCode => ClientProfileSpec {
                id: "claude-code",
                label: "Claude Code",
                default_version: CLAUDE_CODE_VERSION,
                user_agent: CLAUDE_CODE_USER_AGENT,
                headers: CLAUDE_CODE_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
            Self::Codex => ClientProfileSpec {
                id: "codex",
                label: "OpenAI Codex",
                default_version: CODEX_VERSION,
                user_agent: CODEX_USER_AGENT,
                headers: CODEX_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
            Self::Cline => ClientProfileSpec {
                id: "cline",
                label: "Cline",
                default_version: CLINE_VERSION,
                user_agent: CLINE_USER_AGENT,
                headers: CLINE_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
            Self::Cursor => ClientProfileSpec {
                id: "cursor",
                label: "Cursor",
                default_version: CURSOR_VERSION,
                user_agent: CURSOR_USER_AGENT,
                headers: CURSOR_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
            Self::KiloCode => ClientProfileSpec {
                id: "kilo-code",
                label: "Kilo Code",
                default_version: KILO_CODE_VERSION,
                user_agent: KILO_CODE_USER_AGENT,
                headers: KILO_CODE_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
            Self::RooCode => ClientProfileSpec {
                id: "roo-code",
                label: "Roo Code",
                default_version: ROO_CODE_VERSION,
                user_agent: ROO_CODE_USER_AGENT,
                headers: ROO_CODE_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
            Self::Windsurf => ClientProfileSpec {
                id: "windsurf",
                label: "Windsurf",
                default_version: WINDSURF_VERSION,
                user_agent: WINDSURF_USER_AGENT,
                headers: WINDSURF_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
            Self::Aider => ClientProfileSpec {
                id: "aider",
                label: "Aider",
                default_version: AIDER_VERSION,
                user_agent: AIDER_USER_AGENT,
                headers: AIDER_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
            Self::ZCode => ClientProfileSpec {
                id: "zcode",
                label: "ZCode (Z.AI)",
                default_version: ZCODE_VERSION,
                user_agent: ZCODE_USER_AGENT,
                headers: ZCODE_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
            Self::Copilot => ClientProfileSpec {
                id: "copilot",
                label: "GitHub Copilot",
                default_version: "1.107.0",
                user_agent: MUTA_USER_AGENT,
                headers: COPILOT_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
            Self::Antigravity => ClientProfileSpec {
                id: "antigravity",
                label: "Antigravity (Google)",
                default_version: ANTIGRAVITY_VERSION,
                user_agent: ANTIGRAVITY_USER_AGENT,
                headers: ANTIGRAVITY_CLIENT_HEADERS,
                capabilities: ClientCapabilities {
                    coding_platform_compatible: true,
                    has_client_headers: true,
                },
            },
        }
    }

    /// All standard built-in client presets.
    pub const ALL: &'static [ClientPreset] = &[
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

    /// Return all client presets as a slice.
    pub const fn all() -> &'static [ClientPreset] {
        Self::ALL
    }
}

/// First-class client profile presets and custom identity for connection emulation.
#[derive(Debug, Clone, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum ClientProfile {
    /// Default native identity (`User-Agent: muta/<version>`).
    #[default]
    Native,
    /// Emulate OpenCode (`User-Agent: opencode/1.18.18`).
    OpenCode,
    /// Emulate Claude Code (`User-Agent: claude-code/...`).
    ClaudeCode,
    /// Emulate OpenAI Codex (`User-Agent: codex/1.0.0`, `Openai-Intent: conversation-edits`).
    Codex,
    /// Emulate Cline (`User-Agent: Cline/...`, `X-Title: Cline`).
    Cline,
    /// Emulate Cursor (`User-Agent: Cursor/...`, `x-cursor-client-version: ...`).
    Cursor,
    /// Emulate Kilo Code (`User-Agent: Kilo-Code/...`).
    KiloCode,
    /// Emulate Roo Code (`User-Agent: Roo-Code/...`).
    RooCode,
    /// Emulate Windsurf (`User-Agent: Windsurf/...`).
    Windsurf,
    /// Emulate Aider (`User-Agent: aider/...`).
    Aider,
    /// Emulate Zhipu / Z.AI's native ZCode client.
    ZCode,
    /// Emulate GitHub Copilot / VS Code Chat.
    Copilot,
    /// Emulate Google Antigravity (`User-Agent: antigravity/...`).
    Antigravity,
    /// Custom client identity with specific User-Agent and optional headers.
    Custom {
        user_agent: String,
        #[ts(type = "Array<[string, string]>")]
        extra_headers: Vec<(String, String)>,
    },
}

/// Backward compatibility alias for [`ClientProfile`].
pub type ClientIdentity = ClientProfile;

impl ClientProfile {
    /// All standard built-in client profile presets.
    pub const PRESETS: &[ClientProfile] = &[
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

    /// Construct a custom profile with the given User-Agent and optional headers.
    pub fn custom(user_agent: impl Into<String>, extra_headers: Vec<(String, String)>) -> Self {
        Self::Custom {
            user_agent: user_agent.into(),
            extra_headers,
        }
    }

    /// Construct a custom profile that emulates an Antigravity client communicating via remote control proxy.
    pub fn antigravity_remote_control(transport: impl Into<String>) -> Self {
        Self::Custom {
            user_agent: ANTIGRAVITY_USER_AGENT.to_string(),
            extra_headers: vec![
                (ANTIGRAVITY_REMOTE_CONTROL_VIA_HEADER.to_string(), "true".to_string()),
                (ANTIGRAVITY_REMOTE_CONTROL_UA_HEADER.to_string(), ANTIGRAVITY_USER_AGENT.to_string()),
                (ANTIGRAVITY_REMOTE_CONTROL_TRANSPORT_HEADER.to_string(), transport.into()),
                ("x-goog-api-client".to_string(), ANTIGRAVITY_API_CLIENT_HEADER.to_string()),
            ],
        }
    }

    /// Return the corresponding [`ClientPreset`] if this is a standard preset.
    pub fn preset(&self) -> Option<ClientPreset> {
        match self {
            Self::Native => Some(ClientPreset::Native),
            Self::OpenCode => Some(ClientPreset::OpenCode),
            Self::ClaudeCode => Some(ClientPreset::ClaudeCode),
            Self::Codex => Some(ClientPreset::Codex),
            Self::Cline => Some(ClientPreset::Cline),
            Self::Cursor => Some(ClientPreset::Cursor),
            Self::KiloCode => Some(ClientPreset::KiloCode),
            Self::RooCode => Some(ClientPreset::RooCode),
            Self::Windsurf => Some(ClientPreset::Windsurf),
            Self::Aider => Some(ClientPreset::Aider),
            Self::ZCode => Some(ClientPreset::ZCode),
            Self::Copilot => Some(ClientPreset::Copilot),
            Self::Antigravity => Some(ClientPreset::Antigravity),
            Self::Custom { .. } => None,
        }
    }

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
            Self::Custom { .. } => "Custom",
            _ => self
                .preset()
                .map(|p| p.spec().label)
                .unwrap_or("muta (Native)"),
        }
    }

    /// Return the User-Agent header value for this client profile.
    pub fn user_agent(&self) -> &str {
        match self {
            Self::Custom { user_agent, .. } => user_agent.as_str(),
            _ => self
                .preset()
                .map(|p| p.spec().user_agent)
                .unwrap_or(MUTA_USER_AGENT),
        }
    }

    /// Return the client-identity headers for this client profile.
    pub fn headers(&self) -> Vec<(&str, &str)> {
        match self {
            Self::Custom { extra_headers, .. } => extra_headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
            _ => self
                .preset()
                .map(|p| p.spec().headers.to_vec())
                .unwrap_or_default(),
        }
    }

    /// Return the capabilities of this client profile.
    pub fn capabilities(&self) -> ClientCapabilities {
        match self {
            Self::Custom { extra_headers, .. } => ClientCapabilities {
                coding_platform_compatible: false,
                has_client_headers: !extra_headers.is_empty(),
            },
            _ => self
                .preset()
                .map(|p| p.spec().capabilities)
                .unwrap_or_default(),
        }
    }

    /// Parse a preset from an id or common alias.
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "native" | "muta" | "default" => Some(Self::Native),
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
            "antigravity" | "agy" | "google-antigravity" | "jetski" | "jetski-cli" | "cloudcode" | "cloud-code" => {
                Some(Self::Antigravity)
            }
            _ => None,
        }
    }

    /// Resolve a client profile preset from a User-Agent string.
    pub fn from_user_agent(ua: &str) -> Self {
        let trimmed = ua.trim();
        if trimmed.is_empty() || trimmed.starts_with("muta/") {
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
        } else if trimmed.starts_with("antigravity") || trimmed.starts_with("agy") || trimmed.starts_with("jetski") {
            Self::Antigravity
        } else {
            Self::Custom {
                user_agent: trimmed.to_string(),
                extra_headers: Vec::new(),
            }
        }
    }
}

impl From<&str> for ClientProfile {
    fn from(s: &str) -> Self {
        Self::from_user_agent(s)
    }
}

impl From<String> for ClientProfile {
    fn from(s: String) -> Self {
        Self::from_user_agent(&s)
    }
}

impl From<&ClientProfile> for ClientProfile {
    fn from(p: &ClientProfile) -> Self {
        p.clone()
    }
}

impl From<ClientPreset> for ClientProfile {
    fn from(preset: ClientPreset) -> Self {
        match preset {
            ClientPreset::Native => Self::Native,
            ClientPreset::OpenCode => Self::OpenCode,
            ClientPreset::ClaudeCode => Self::ClaudeCode,
            ClientPreset::Codex => Self::Codex,
            ClientPreset::Cline => Self::Cline,
            ClientPreset::Cursor => Self::Cursor,
            ClientPreset::KiloCode => Self::KiloCode,
            ClientPreset::RooCode => Self::RooCode,
            ClientPreset::Windsurf => Self::Windsurf,
            ClientPreset::Aider => Self::Aider,
            ClientPreset::ZCode => Self::ZCode,
            ClientPreset::Copilot => Self::Copilot,
            ClientPreset::Antigravity => Self::Antigravity,
        }
    }
}

impl Serialize for ClientProfile {
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

impl<'de> Deserialize<'de> for ClientProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ClientProfileVisitor;

        impl<'de> serde::de::Visitor<'de> for ClientProfileVisitor {
            type Value = ClientProfile;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a client identity preset string or custom identity object")
            }

            fn visit_str<E>(self, value: &str) -> Result<ClientProfile, E>
            where
                E: serde::de::Error,
            {
                ClientProfile::from_id(value)
                    .or_else(|| Some(ClientProfile::from_user_agent(value)))
                    .ok_or_else(|| E::custom(format!("unknown client identity preset: {value}")))
            }

            fn visit_map<M>(self, mut map: M) -> Result<ClientProfile, M::Error>
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
                let user_agent =
                    user_agent.ok_or_else(|| serde::de::Error::missing_field("user_agent"))?;
                Ok(ClientProfile::Custom {
                    user_agent,
                    extra_headers,
                })
            }
        }

        deserializer.deserialize_any(ClientProfileVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_specs_cover_all_presets() {
        for preset in ClientPreset::all() {
            let spec = preset.spec();
            assert!(!spec.id.is_empty());
            assert!(!spec.label.is_empty());
            assert!(!spec.user_agent.is_empty());

            let profile: ClientProfile = (*preset).into();
            assert_eq!(profile.id(), spec.id);
            assert_eq!(profile.label(), spec.label);
            assert_eq!(profile.user_agent(), spec.user_agent);

            let parsed = ClientProfile::from_id(spec.id).expect("resolves from id");
            assert_eq!(parsed, profile);
        }
    }

    #[test]
    fn preset_headers_and_capabilities() {
        let zcode = ClientProfile::ZCode;
        assert!(zcode.capabilities().has_client_headers);
        assert!(zcode.capabilities().coding_platform_compatible);
        let headers = zcode.headers();
        assert!(headers.iter().any(|(k, v)| *k == "X-Title" && *v == "Z Code"));
        assert!(headers.iter().any(|(k, v)| *k == "X-ZCode-Agent" && *v == "glm"));

        let claude = ClientProfile::ClaudeCode;
        assert!(claude.headers().iter().any(|(k, v)| *k == "x-app" && *v == "claude-code"));

        let agy = ClientProfile::Antigravity;
        assert!(agy.headers().iter().any(|(k, v)| *k == "x-goog-api-client" && *v == "gl-go/1.23.2 gdcl/0.1"));

        let cline = ClientProfile::Cline;
        assert!(cline.headers().iter().any(|(k, v)| *k == "X-Title" && *v == "Cline"));

        let cursor = ClientProfile::Cursor;
        assert!(cursor.headers().iter().any(|(k, v)| *k == "X-Title" && *v == "Cursor"));
    }

    #[test]
    fn custom_profile_preserves_custom_headers() {
        let custom = ClientProfile::custom(
            "my-custom-ua/1.0",
            vec![
                ("X-Custom-Token".to_string(), "secret123".to_string()),
                ("X-Client-Version".to_string(), "2.0".to_string()),
            ],
        );
        assert_eq!(custom.user_agent(), "my-custom-ua/1.0");
        let headers = custom.headers();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0], ("X-Custom-Token", "secret123"));
        assert_eq!(headers[1], ("X-Client-Version", "2.0"));
    }

    #[test]
    fn serde_json_roundtrip() {
        let preset = ClientProfile::ClaudeCode;
        let json = serde_json::to_string(&preset).expect("serialize preset");
        assert_eq!(json, "\"claude-code\"");
        let deserialized: ClientProfile = serde_json::from_str(&json).expect("deserialize preset");
        assert_eq!(deserialized, ClientProfile::ClaudeCode);

        // Also accepts CamelCase and alias forms
        let from_alias: ClientProfile = serde_json::from_str("\"ClaudeCode\"").expect("deserialize alias");
        assert_eq!(from_alias, ClientProfile::ClaudeCode);

        let custom = ClientProfile::custom(
            "agent/1.0",
            vec![("X-Custom".to_string(), "val".to_string())],
        );
        let custom_json = serde_json::to_string(&custom).expect("serialize custom");
        let custom_deserialized: ClientProfile = serde_json::from_str(&custom_json).expect("deserialize custom");
        assert_eq!(custom_deserialized.user_agent(), "agent/1.0");
        assert_eq!(custom_deserialized.headers(), vec![("X-Custom", "val")]);
    }

    #[test]
    fn antigravity_identity_and_metadata() {
        assert_eq!(ClientProfile::from_id("agy"), Some(ClientProfile::Antigravity));
        assert_eq!(ClientProfile::from_id("jetski"), Some(ClientProfile::Antigravity));
        assert_eq!(ClientProfile::from_id("jetski-cli"), Some(ClientProfile::Antigravity));
        assert_eq!(ClientProfile::from_id("cloudcode"), Some(ClientProfile::Antigravity));
        assert_eq!(ClientProfile::from_user_agent("jetski/1.23.2 linux/amd64"), ClientProfile::Antigravity);

        let meta = AntigravityClientMetadata::new("test-session-123", "fp-abcd-5678", "SSH session");
        assert_eq!(meta.ide_name, "antigravity");
        assert_eq!(meta.ide_version, ANTIGRAVITY_VERSION);
        assert_eq!(meta.session_id, "test-session-123");
        assert_eq!(meta.device_fingerprint, "fp-abcd-5678");
        assert_eq!(meta.runtime_environment, "SSH session");
        assert!(meta.user_agent().starts_with("antigravity/"));

        let remote = ClientProfile::antigravity_remote_control("webchannel");
        let headers = remote.headers();
        assert!(headers.iter().any(|(k, v)| *k == ANTIGRAVITY_REMOTE_CONTROL_VIA_HEADER && *v == "true"));
        assert!(headers.iter().any(|(k, v)| *k == ANTIGRAVITY_REMOTE_CONTROL_TRANSPORT_HEADER && *v == "webchannel"));
        assert!(headers.iter().any(|(k, v)| *k == "x-goog-api-client" && *v == ANTIGRAVITY_API_CLIENT_HEADER));
    }
}
