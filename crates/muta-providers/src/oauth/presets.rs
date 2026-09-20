//! Official OAuth 2.0 client presets and credentials for subscription providers.
//!
//! Pinned and maintained in `muta-providers` (not `muta-contracts`), keeping core
//! domain contracts 100% free of vendor-specific secrets, client IDs, and endpoints.

use muta_contracts::LoginMethod;
use muta_contracts::provider_auth::{
    ClientAuthMethod, DeviceFlowMode, OAuthConfig, PkceMode, PortMode, TokenRequestFormat,
};
use std::borrow::Cow;

pub const GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_ID: &str = concat!(
    "1071006060591-",
    "tmhssin2h21lcre235vtolojh4g403ep",
    ".apps.googleusercontent.com"
);

pub const GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_SECRET: &str =
    concat!("GOCSPX-", "K58FWR486LdLJ1mLB8sXC4z6qDAf");

pub const GOOGLE_ANTIGRAVITY_CLI_CLIENT_ID: &str = concat!(
    "670498708453-",
    "kgn8ok56m62g1hh8smlf5geh4ck4oq0s",
    ".apps.googleusercontent.com"
);

pub const GOOGLE_ANTIGRAVITY_CLI_CLIENT_SECRET: &str =
    concat!("GOCSPX-", "m-p1-s6mkmWz_a_iUqUo2E7J3qY9");

pub const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const CHATGPT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const COPILOT_CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";
pub const QODER_CLIENT_ID: &str = "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb";
pub const QODER_CN_CLIENT_ID: &str = "e93fe488-5778-4c35-a6fc-0f54ed7b3139";
pub const OPENCODE_CLIENT_ID: &str = "opencode-cli";
pub const OPENCODE_CONSOLE_URL: &str = "https://opencode.ai/console";

/// Google Antigravity (Cloud Code Companion) preset.
pub fn google_antigravity_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("google-antigravity"),
        client_id: Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_ID),
        client_secret: Some(Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_SECRET)),
        client_auth_method: ClientAuthMethod::RequestBody,
        authorize_url: Cow::Borrowed("https://accounts.google.com/o/oauth2/v2/auth"),
        token_url: Cow::Borrowed("https://oauth2.googleapis.com/token"),
        device_authorization_url: Cow::Borrowed("https://oauth2.googleapis.com/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed(
            "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs openid",
        ),
        extra_authorize_params: vec![
            (Cow::Borrowed("access_type"), Cow::Borrowed("offline")),
            (Cow::Borrowed("prompt"), Cow::Borrowed("consent")),
            (
                Cow::Borrowed("include_granted_scopes"),
                Cow::Borrowed("true"),
            ),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: Some(Cow::Borrowed(
            muta_contracts::client_identity::ANTIGRAVITY_USER_AGENT,
        )),
        browser_login: true,
        default_login_method: LoginMethod::Browser,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 51121,
        port_mode: PortMode::PreferredOrDynamic(51121),
        oauth_path: Cow::Borrowed("/oauth-callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::disabled(),
        device_token_url: Cow::Borrowed("https://oauth2.googleapis.com/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// Google Antigravity Standalone CLI preset.
pub fn google_antigravity_cli_preset() -> OAuthConfig {
    let mut cfg = google_antigravity_preset();
    cfg.provider_id = Cow::Borrowed("antigravity-cli");
    cfg.client_id = Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLI_CLIENT_ID);
    cfg.client_secret = Some(Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLI_CLIENT_SECRET));
    cfg
}

/// xAI SuperGrok preset.
pub fn xai_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("xai"),
        client_id: Cow::Borrowed(XAI_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://auth.x.ai/oauth2/authorize"),
        token_url: Cow::Borrowed("https://auth.x.ai/oauth2/token"),
        device_authorization_url: Cow::Borrowed("https://auth.x.ai/oauth2/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed("openid profile email offline_access grok-cli:access api:access"),
        extra_authorize_params: vec![
            (Cow::Borrowed("plan"), Cow::Borrowed("generic")),
            (Cow::Borrowed("referrer"), Cow::Borrowed("muta")),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: true,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 56121,
        port_mode: PortMode::Fixed(56121),
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: true,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::rfc8628(),
        device_token_url: Cow::Borrowed("https://auth.x.ai/oauth2/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// OpenAI / ChatGPT Subscription preset.
pub fn chatgpt_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("chatgpt"),
        client_id: Cow::Borrowed(CHATGPT_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://auth.openai.com/oauth/authorize"),
        token_url: Cow::Borrowed("https://auth.openai.com/oauth/token"),
        device_authorization_url: Cow::Borrowed(
            "https://auth.openai.com/api/accounts/deviceauth/usercode",
        ),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed(
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        ),
        extra_authorize_params: vec![
            (
                Cow::Borrowed("id_token_add_organizations"),
                Cow::Borrowed("true"),
            ),
            (
                Cow::Borrowed("codex_cli_simplified_flow"),
                Cow::Borrowed("true"),
            ),
            (Cow::Borrowed("originator"), Cow::Borrowed("codex_cli_rs")),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: true,
        default_login_method: LoginMethod::Browser,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 1455,
        port_mode: PortMode::Fixed(1455),
        oauth_path: Cow::Borrowed("/auth/callback"),
        redirect_host: Cow::Borrowed("localhost"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::custom("chatgpt"),
        device_token_url: Cow::Borrowed("https://auth.openai.com/api/accounts/deviceauth/token"),
        device_redirect_uri: Cow::Borrowed("https://auth.openai.com/deviceauth/callback"),
    }
}

/// GitHub Copilot subscription preset.
pub fn copilot_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("copilot"),
        client_id: Cow::Borrowed(COPILOT_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://github.com/login/oauth/authorize"),
        token_url: Cow::Borrowed("https://github.com/login/oauth/access_token"),
        device_authorization_url: Cow::Borrowed("https://github.com/login/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed("read:user"),
        extra_authorize_params: Vec::new(),
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: false,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 42195,
        port_mode: PortMode::Fixed(42195),
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::rfc8628(),
        device_token_url: Cow::Borrowed("https://github.com/login/oauth/access_token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// Alibaba Qoder international preset.
pub fn qoder_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("qoder"),
        client_id: Cow::Borrowed(QODER_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://qoder.com/device/selectAccounts"),
        token_url: Cow::Borrowed("https://openapi.qoder.sh/api/v1/deviceToken/poll"),
        device_authorization_url: Cow::Borrowed("https://qoder.com/device/selectAccounts"),
        grant_type_device: Cow::Borrowed("qoder_device_flow"),
        scope: Cow::Borrowed("openid"),
        extra_authorize_params: Vec::new(),
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: false,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 0,
        port_mode: PortMode::Dynamic,
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: true,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::Json,
        device_flow: DeviceFlowMode::custom("qoder"),
        device_token_url: Cow::Borrowed("https://openapi.qoder.sh/api/v1/deviceToken/poll"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// Alibaba Qoder CN region preset.
pub fn qoder_cn_preset() -> OAuthConfig {
    let mut cfg = qoder_preset();
    cfg.provider_id = Cow::Borrowed("qoder-cn");
    cfg.client_id = Cow::Borrowed(QODER_CN_CLIENT_ID);
    cfg.authorize_url = Cow::Borrowed("https://qoder.com.cn/device/selectAccounts");
    cfg.token_url = Cow::Borrowed("https://openapi.qoder.com.cn/api/v1/deviceToken/poll");
    cfg.device_authorization_url = Cow::Borrowed("https://qoder.com.cn/device/selectAccounts");
    cfg.device_token_url =
        Cow::Borrowed("https://openapi.qoder.com.cn/api/v1/deviceToken/poll");
    cfg
}

/// OpenCode Console account preset (the OpenCode Go / Zen subscription).
///
/// OpenCode uses a JSON device-authorization grant: the `/auth/device/code`
/// endpoint takes `{client_id}` and the `/auth/device/token` endpoint both
/// polls for the initial tokens and rotates them on refresh. There is no
/// loopback browser callback and no scope.
pub fn opencode_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("opencode"),
        client_id: Cow::Borrowed(OPENCODE_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed(OPENCODE_CONSOLE_URL),
        token_url: Cow::Borrowed("https://opencode.ai/console/auth/device/token"),
        device_authorization_url: Cow::Borrowed("https://opencode.ai/console/auth/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed(""),
        extra_authorize_params: Vec::new(),
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: false,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 0,
        port_mode: PortMode::Dynamic,
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::Disabled,
        token_format: TokenRequestFormat::Json,
        device_flow: DeviceFlowMode::custom("opencode"),
        device_token_url: Cow::Borrowed("https://opencode.ai/console/auth/device/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// Lookup OAuth preset configuration by stable provider id.
pub fn config_by_provider_id(provider_id: &str) -> Option<OAuthConfig> {
    match provider_id {
        "google-antigravity" | "antigravity" => Some(google_antigravity_preset()),
        "antigravity-cli" => Some(google_antigravity_cli_preset()),
        "xai" => Some(xai_preset()),
        "chatgpt" | "openai-subscription" => Some(chatgpt_preset()),
        "copilot" | "github-copilot" => Some(copilot_preset()),
        "qoder" => Some(qoder_preset()),
        "qoder-cn" => Some(qoder_cn_preset()),
        "opencode" | "opencode-go" => Some(opencode_preset()),
        _ => None,
    }
}

pub fn is_qoder(config: &OAuthConfig) -> bool {
    config.provider_id == "qoder"
        || config.provider_id == "qoder-cn"
        || config.token_url.contains("openapi.qoder.sh")
        || config.token_url.contains("openapi.qoder.com.cn")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_preset_is_device_only_json() {
        let cfg = config_by_provider_id("opencode").expect("opencode preset");
        assert_eq!(cfg.client_id, OPENCODE_CLIENT_ID);
        assert!(!cfg.browser_login);
        assert_eq!(
            cfg.effective_default_login_method(),
            Some(LoginMethod::Device)
        );
        assert!(cfg.supports_login_method(LoginMethod::Device));
        assert!(!cfg.supports_login_method(LoginMethod::Browser));
        assert_eq!(cfg.device_flow, DeviceFlowMode::custom("opencode"));
        assert_eq!(cfg.token_format, TokenRequestFormat::Json);
        assert_eq!(cfg.pkce_mode, PkceMode::Disabled);
        assert_eq!(
            cfg.device_authorization_url,
            "https://opencode.ai/console/auth/device/code"
        );
        assert_eq!(
            cfg.token_url,
            "https://opencode.ai/console/auth/device/token"
        );
    }

    #[test]
    fn opencode_go_alias_resolves_the_same_preset() {
        assert_eq!(
            config_by_provider_id("opencode-go").map(|c| c.provider_id),
            Some(Cow::Borrowed("opencode"))
        );
    }
}
