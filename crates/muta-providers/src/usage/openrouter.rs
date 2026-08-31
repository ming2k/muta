//! OpenRouter balance / credit limit / rate limit fetcher.
//!
//! Query endpoint: `GET https://openrouter.ai/api/v1/auth/key`

use muta_contracts::async_trait;
use muta_contracts::{ProviderUsage, UsageMetric};
use serde::Deserialize;

use super::ProviderUsageFetcher;

#[derive(Debug, Deserialize)]
pub(crate) struct OpenRouterKeyResponse {
    #[serde(default)]
    pub(crate) data: Option<OpenRouterKeyData>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenRouterKeyData {
    #[serde(default)]
    pub(crate) label: Option<String>,
    #[serde(default)]
    pub(crate) usage: f64,
    #[serde(default)]
    pub(crate) limit: Option<f64>,
    #[serde(default)]
    pub(crate) is_free_tier: bool,
    #[serde(default)]
    pub(crate) rate_limit: Option<OpenRouterRateLimit>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenRouterRateLimit {
    #[serde(default)]
    pub(crate) requests: i64,
    #[serde(default)]
    pub(crate) interval: String,
}

pub struct OpenRouterUsageFetcher;

#[async_trait]
impl ProviderUsageFetcher for OpenRouterUsageFetcher {
    fn matches(&self, preset_id: Option<&str>, base_url: &str) -> bool {
        preset_id == Some("openrouter") || base_url.contains("openrouter.ai")
    }

    async fn fetch_usage(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        let endpoint = if base_url.contains("openrouter.ai") {
            let base = base_url.trim_end_matches('/');
            let base = base.strip_suffix("/api/v1").unwrap_or(base);
            let base = base.strip_suffix("/v1").unwrap_or(base);
            format!("{base}/api/v1/auth/key")
        } else {
            "https://openrouter.ai/api/v1/auth/key".to_string()
        };

        let resp = client
            .get(&endpoint)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"))
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("HTTP {status}: {text}"));
        }

        let body: OpenRouterKeyResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse OpenRouter key response: {e}"))?;

        parse_openrouter_key(body)
    }
}

pub(crate) fn parse_openrouter_key(body: OpenRouterKeyResponse) -> Result<ProviderUsage, String> {
    let data = body.data.ok_or_else(|| {
        "No data returned by OpenRouter auth/key API".to_string()
    })?;

    let plan = if data.is_free_tier {
        Some("Free Tier".to_string())
    } else {
        Some("Pay-as-you-go".to_string())
    };

    let primary_balance = if let Some(limit) = data.limit {
        format!("${:.2} / ${:.2}", data.usage, limit)
    } else {
        format!("${:.2} used", data.usage)
    };

    let mut metrics = Vec::new();
    if let Some(lbl) = data.label && !lbl.is_empty() {
        metrics.push(UsageMetric {
            label: "Key Label".to_string(),
            value: lbl,
            unit: None,
        });
    }
    metrics.push(UsageMetric {
        label: "Usage".to_string(),
        value: format!("${:.4}", data.usage),
        unit: Some("USD".to_string()),
    });
    if let Some(limit) = data.limit {
        metrics.push(UsageMetric {
            label: "Credit Limit".to_string(),
            value: format!("${:.2}", limit),
            unit: Some("USD".to_string()),
        });
    }
    if let Some(rl) = data.rate_limit {
        metrics.push(UsageMetric {
            label: "Rate Limit".to_string(),
            value: format!("{} req / {}", rl.requests, rl.interval),
            unit: None,
        });
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .ok();

    Ok(ProviderUsage {
        plan,
        primary_balance: Some(primary_balance),
        metrics,
        updated_at_ms: now_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openrouter_key_json() {
        let json = r#"{
            "data": {
                "label": "Work project",
                "usage": 1.2345,
                "limit": 10.0,
                "is_free_tier": false,
                "rate_limit": {
                    "requests": 200,
                    "interval": "10s"
                }
            }
        }"#;

        let parsed: OpenRouterKeyResponse = serde_json::from_str(json).unwrap();
        let usage = parse_openrouter_key(parsed).unwrap();

        assert_eq!(usage.plan.as_deref(), Some("Pay-as-you-go"));
        assert_eq!(usage.primary_balance.as_deref(), Some("$1.23 / $10.00"));
        assert_eq!(usage.metrics.len(), 4);
        assert_eq!(usage.metrics[0].label, "Key Label");
        assert_eq!(usage.metrics[0].value, "Work project");
    }
}
