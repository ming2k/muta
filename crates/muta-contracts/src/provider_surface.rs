//! Provider service roots and catalog declarations shared by configuration and routing.

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ApiRoot(String);

impl ApiRoot {
    pub fn parse(value: &str) -> Result<Self, String> {
        let url = url::Url::parse(value).map_err(|e| format!("invalid API root: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("API root must be an absolute HTTP(S) URL".into());
        }
        if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err("API root must not contain credentials, a query, or a fragment".into());
        }
        Ok(Self(url.as_str().trim_end_matches('/').to_string()))
    }

    pub fn as_str(&self) -> &str { &self.0 }

    /// Append a protocol-defined relative path without stripping or replacing root segments.
    pub fn append(&self, path: &str) -> String {
        format!("{}/{}", self.0, path)
    }
}

impl TryFrom<String> for ApiRoot {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> { Self::parse(&value) }
}
impl From<ApiRoot> for String {
    fn from(value: ApiRoot) -> Self { value.0 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoveryProtocol {
    #[serde(rename = "openai")]
    OpenAi,
    Anthropic,
    Google,
    GoogleCloudCode,
    Codex,
    OpencodeGo,
}
impl DiscoveryProtocol {
    pub fn from_wire_protocol(protocol: crate::WireProtocol) -> Self {
        match protocol {
            crate::WireProtocol::AnthropicMessages => Self::Anthropic,
            crate::WireProtocol::GoogleGemini => Self::Google,
            crate::WireProtocol::ChatCompletions | crate::WireProtocol::Responses => Self::OpenAi,
        }
    }
    pub fn path(self) -> &'static str {
        match self {
            Self::GoogleCloudCode => "v1internal:fetchAvailableModels",
            Self::OpencodeGo => "api.json",
            _ => "models",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "source", content = "format", rename_all = "kebab-case")]
pub enum RemoteCatalogSource {
    Endpoint(DiscoveryProtocol),
    None,
}

/// Declarative cache capabilities, with exact model exceptions over a default.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderPromptCache {
    pub default: Option<crate::PromptCacheCapabilities>,
    pub models: std::collections::BTreeMap<String, crate::PromptCacheCapabilities>,
}
impl ProviderPromptCache {
    pub fn resolve(&self, model: &str) -> crate::PromptCacheCapabilities {
        self.models.get(model).or(self.default.as_ref()).cloned()
            .unwrap_or_else(crate::PromptCacheCapabilities::unsupported)
    }
    pub fn validate(&self) -> Result<(), String> {
        for capabilities in self.default.iter().chain(self.models.values()) {
            capabilities.validate().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
