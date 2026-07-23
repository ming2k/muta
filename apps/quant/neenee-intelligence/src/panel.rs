//! Multi-perspective AI expert meetings with a dedicated meeting manager.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::join_all;
use neenee_core::{Message, ModelRequest, Role};
use neenee_store::cache::CachedResource;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_MEETINGS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewScenario {
    InvestmentThesis,
    MarketEvent,
    TradeRisk,
    StrategyReview,
}

impl ReviewScenario {
    pub fn label(self) -> &'static str {
        match self {
            Self::InvestmentThesis => "Investment thesis",
            Self::MarketEvent => "Market event",
            Self::TradeRisk => "Trade risk",
            Self::StrategyReview => "Strategy review",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpertRole {
    pub id: String,
    pub name: String,
    pub mandate: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpertContribution {
    pub expert_id: String,
    pub expert_name: String,
    pub round: u8,
    pub stance: String,
    pub confidence: f32,
    pub analysis: String,
    pub risks: Vec<String>,
    pub evidence_gaps: Vec<String>,
    pub challenges: Vec<String>,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeetingConclusion {
    pub recommendation: String,
    pub confidence: f32,
    pub consensus: Vec<String>,
    pub disagreements: Vec<String>,
    pub actions: Vec<String>,
    pub stop_conditions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    Complete,
    Degraded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpertMeeting {
    pub id: String,
    pub topic: String,
    pub context: String,
    pub scenario: ReviewScenario,
    pub provider: String,
    pub model: String,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub status: MeetingStatus,
    pub contributions: Vec<ExpertContribution>,
    pub conclusion: MeetingConclusion,
}

#[async_trait]
pub trait AiCompletion: Send + Sync {
    async fn complete(&self, system: &str, user: &str) -> Result<String, String>;
    fn provider(&self) -> String;
    fn model(&self) -> String;
}

struct NeeneeGateway {
    config: neenee_store::config::Config,
    provider_id: String,
    model_id: Option<String>,
}

#[async_trait]
impl AiCompletion for NeeneeGateway {
    async fn complete(&self, system: &str, user: &str) -> Result<String, String> {
        // Build a fresh provider for every participant. Some transports retain
        // conversation state, so sharing one instance across concurrent expert
        // calls could accidentally mix identities or response chains. The
        // catalog returns `None` when the id is unknown or has no resolvable
        // channel — refuse explicitly instead of letting the call reach a
        // non-functional placeholder.
        let Some(provider) = neenee_agent::catalog::build_provider_for_model(
            &self.config,
            &self.provider_id,
            self.model_id.as_deref(),
            None,
        ) else {
            return Err("no configured AI provider is available for expert review".to_string());
        };
        provider
            .chat(ModelRequest::new(vec![
                Message::new(Role::System, system),
                Message::new(Role::User, user),
            ]))
            .await
            .map(|message| message.content)
    }

    fn provider(&self) -> String {
        self.provider_id.clone()
    }

    fn model(&self) -> String {
        self.model_id
            .clone()
            .unwrap_or_else(|| "provider default".to_string())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MeetingArchive {
    meetings: Vec<ExpertMeeting>,
}

pub struct ExpertPanel {
    gateway: Arc<dyn AiCompletion>,
    cache: CachedResource,
    meetings: Vec<ExpertMeeting>,
}

impl ExpertPanel {
    pub fn system_default() -> Result<Self, String> {
        let config = neenee_store::config::Config::load();
        let entries = neenee_agent::catalog::build_catalog(&config);
        let provider_id = if config.default_provider.trim().is_empty() {
            entries.first().map(|entry| entry.id.clone())
        } else {
            Some(config.default_provider.clone())
        }
        .ok_or_else(|| {
            "configure an AI provider in neenee before starting an expert meeting".to_string()
        })?;
        if !entries.iter().any(|entry| entry.id == provider_id) {
            return Err(format!(
                "configured expert provider '{provider_id}' is not available"
            ));
        }
        let gateway = Arc::new(NeeneeGateway {
            model_id: config.default_model.clone(),
            config,
            provider_id,
        });
        let path = neenee_store::paths::get()
            .state_dir
            .join("intelligence")
            .join("expert-meetings.json");
        Ok(Self::with_gateway(path, gateway))
    }

    pub fn with_gateway(path: PathBuf, gateway: Arc<dyn AiCompletion>) -> Self {
        let cache = CachedResource::new(path);
        let meetings = cache
            .load_json::<MeetingArchive>()
            .unwrap_or_default()
            .meetings;
        Self {
            gateway,
            cache,
            meetings,
        }
    }

    pub fn meetings(&self) -> &[ExpertMeeting] {
        &self.meetings
    }

    pub async fn convene(
        &mut self,
        scenario: ReviewScenario,
        topic: &str,
        context: &str,
    ) -> Result<&ExpertMeeting, String> {
        let topic = topic.trim();
        if topic.is_empty() {
            return Err("expert meeting topic must not be empty".to_string());
        }
        let context = context.trim();
        let started_at_ms = unix_now_ms();
        let roles = roles_for(scenario);

        let first_round = join_all(roles.iter().map(|role| {
            run_expert(
                Arc::clone(&self.gateway),
                role.clone(),
                1,
                topic.to_string(),
                context.to_string(),
                String::new(),
            )
        }))
        .await;
        let first_transcript = serde_json::to_string(&first_round)
            .map_err(|error| format!("serialize first expert round: {error}"))?;
        let second_round = join_all(roles.iter().map(|role| {
            run_expert(
                Arc::clone(&self.gateway),
                role.clone(),
                2,
                topic.to_string(),
                context.to_string(),
                first_transcript.clone(),
            )
        }))
        .await;

        let mut contributions = first_round;
        contributions.extend(second_round);
        let failed_expert = contributions.iter().any(|contribution| contribution.failed);
        let manager_input = serde_json::to_string(&contributions)
            .map_err(|error| format!("serialize expert transcript: {error}"))?;
        let manager_response = self
            .gateway
            .complete(
                manager_system_prompt(),
                &format!(
                    "Scenario: {}\nTopic: {topic}\nContext: {context}\n\nTwo-round expert transcript:\n{manager_input}",
                    scenario.label()
                ),
            )
            .await;
        let (conclusion, manager_degraded) = match manager_response {
            Ok(raw) => match parse_conclusion(&raw) {
                Ok(conclusion) => (conclusion, false),
                Err(error) => (fallback_conclusion(&error), true),
            },
            Err(error) => (fallback_conclusion(&error), true),
        };
        let meeting = ExpertMeeting {
            id: Uuid::new_v4().to_string(),
            topic: topic.to_string(),
            context: context.to_string(),
            scenario,
            provider: self.gateway.provider(),
            model: self.gateway.model(),
            started_at_ms,
            completed_at_ms: unix_now_ms(),
            status: if failed_expert || manager_degraded {
                MeetingStatus::Degraded
            } else {
                MeetingStatus::Complete
            },
            contributions,
            conclusion,
        };
        self.meetings.insert(0, meeting);
        self.meetings.truncate(MAX_MEETINGS);
        self.persist()?;
        self.meetings
            .first()
            .ok_or_else(|| "expert meeting was not retained".to_string())
    }

    fn persist(&self) -> Result<(), String> {
        self.cache.store_json(&MeetingArchive {
            meetings: self.meetings.clone(),
        })
    }
}

async fn run_expert(
    gateway: Arc<dyn AiCompletion>,
    role: ExpertRole,
    round: u8,
    topic: String,
    context: String,
    peer_transcript: String,
) -> ExpertContribution {
    let (system, user) = if round == 1 {
        (
            format!(
                "You are {} in round one of a decision review. {} Work independently. Separate verified evidence, assumptions, and inference. Return only JSON with keys stance, confidence (0..1), analysis, risks (array), evidence_gaps (array), challenges (array).",
                role.name, role.mandate
            ),
            format!("Topic: {topic}\nContext: {context}"),
        )
    } else {
        (
            format!(
                "You are {} in round two of a decision review. {} Read every first-round opinion, challenge weak assumptions, answer criticism relevant to your mandate, and revise your position when warranted. Return only JSON with keys stance, confidence (0..1), analysis, risks (array), evidence_gaps (array), challenges (array).",
                role.name, role.mandate
            ),
            format!(
                "Topic: {topic}\nContext: {context}\n\nFirst-round peer opinions:\n{peer_transcript}"
            ),
        )
    };
    match gateway.complete(&system, &user).await {
        Ok(raw) => parse_contribution(&role, round, &raw),
        Err(error) => failed_contribution(&role, round, error),
    }
}

fn parse_contribution(role: &ExpertRole, round: u8, raw: &str) -> ExpertContribution {
    #[derive(Deserialize)]
    #[serde(default)]
    struct Payload {
        stance: String,
        confidence: f32,
        analysis: String,
        risks: Vec<String>,
        evidence_gaps: Vec<String>,
        challenges: Vec<String>,
    }

    impl Default for Payload {
        fn default() -> Self {
            Self {
                stance: "uncertain".to_string(),
                confidence: 0.0,
                analysis: String::new(),
                risks: Vec::new(),
                evidence_gaps: Vec::new(),
                challenges: Vec::new(),
            }
        }
    }

    match serde_json::from_str::<Payload>(strip_code_fence(raw)) {
        Ok(payload) => ExpertContribution {
            expert_id: role.id.clone(),
            expert_name: role.name.clone(),
            round,
            stance: payload.stance,
            confidence: payload.confidence.clamp(0.0, 1.0),
            analysis: payload.analysis,
            risks: payload.risks,
            evidence_gaps: payload.evidence_gaps,
            challenges: payload.challenges,
            failed: false,
        },
        Err(error) => failed_contribution(
            role,
            round,
            format!("invalid expert response ({error}): {}", truncate(raw, 300)),
        ),
    }
}

fn failed_contribution(role: &ExpertRole, round: u8, error: String) -> ExpertContribution {
    ExpertContribution {
        expert_id: role.id.clone(),
        expert_name: role.name.clone(),
        round,
        stance: "unavailable".to_string(),
        confidence: 0.0,
        analysis: error,
        risks: Vec::new(),
        evidence_gaps: vec!["expert response unavailable".to_string()],
        challenges: Vec::new(),
        failed: true,
    }
}

fn parse_conclusion(raw: &str) -> Result<MeetingConclusion, String> {
    #[derive(Deserialize)]
    struct Payload {
        recommendation: String,
        confidence: f32,
        #[serde(default)]
        consensus: Vec<String>,
        #[serde(default)]
        disagreements: Vec<String>,
        #[serde(default)]
        actions: Vec<String>,
        #[serde(default)]
        stop_conditions: Vec<String>,
    }

    let payload = serde_json::from_str::<Payload>(strip_code_fence(raw))
        .map_err(|error| format!("meeting manager returned invalid JSON: {error}"))?;
    if payload.recommendation.trim().is_empty() {
        return Err("meeting manager returned an empty recommendation".to_string());
    }
    Ok(MeetingConclusion {
        recommendation: payload.recommendation,
        confidence: payload.confidence.clamp(0.0, 1.0),
        consensus: payload.consensus,
        disagreements: payload.disagreements,
        actions: payload.actions,
        stop_conditions: payload.stop_conditions,
    })
}

fn fallback_conclusion(error: &str) -> MeetingConclusion {
    MeetingConclusion {
        recommendation: format!("No reliable meeting conclusion: {error}"),
        confidence: 0.0,
        consensus: Vec::new(),
        disagreements: vec!["The meeting manager could not synthesize the panel".to_string()],
        actions: vec!["Review individual expert contributions manually".to_string()],
        stop_conditions: vec![
            "Do not treat this degraded meeting as an execution signal".to_string(),
        ],
    }
}

fn manager_system_prompt() -> &'static str {
    "You are the meeting manager for a multi-expert decision review. You do not vote and you do not hide disagreement. Evaluate evidence quality, distinguish consensus from correlated assumptions, penalize missing data, and produce a bounded conclusion. Return only JSON with keys recommendation, confidence (0..1), consensus (array), disagreements (array), actions (array), stop_conditions (array). Never turn the conclusion into an automatic trade instruction."
}

fn roles_for(scenario: ReviewScenario) -> Vec<ExpertRole> {
    let scenario_focus = match scenario {
        ReviewScenario::InvestmentThesis => {
            "Test whether the investment thesis is causal, priced, and falsifiable."
        }
        ReviewScenario::MarketEvent => {
            "Assess event credibility, transmission paths, timing, and market reflexivity."
        }
        ReviewScenario::TradeRisk => {
            "Assess sizing, liquidity, tail exposure, execution, and explicit invalidation."
        }
        ReviewScenario::StrategyReview => {
            "Assess data leakage, regime dependence, costs, robustness, and operational failure."
        }
    };
    [
        (
            "fundamental",
            "Fundamental analyst",
            "Focus on business economics, valuation, primary evidence, and what is already priced.",
        ),
        (
            "macro",
            "Macro strategist",
            "Focus on rates, liquidity, policy, currencies, cross-asset transmission, and regime shifts.",
        ),
        (
            "microstructure",
            "Market microstructure specialist",
            "Focus on positioning, flows, liquidity, execution quality, crowding, and time horizon.",
        ),
        (
            "risk",
            "Risk officer",
            "Assume the proposal can fail. Quantify downside paths, concentration, correlations, and stop conditions.",
        ),
        (
            "contrarian",
            "Contrarian evidence auditor",
            "Challenge source quality, narrative consensus, hidden assumptions, selection bias, and alternative explanations.",
        ),
    ]
    .into_iter()
    .map(|(id, name, mandate)| ExpertRole {
        id: id.to_string(),
        name: name.to_string(),
        mandate: format!("{mandate} {scenario_focus}"),
    })
    .collect()
}

fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let content = rest
        .find('\n')
        .map(|index| &rest[index + 1..])
        .unwrap_or(rest);
    content
        .rfind("```")
        .map(|index| content[..index].trim())
        .unwrap_or(content.trim())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn unix_now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeGateway {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl AiCompletion for FakeGateway {
        async fn complete(&self, system: &str, _user: &str) -> Result<String, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if system.contains("meeting manager") {
                Ok(r#"{"recommendation":"Proceed only after validation","confidence":0.72,"consensus":["evidence matters"],"disagreements":["timing"],"actions":["validate data"],"stop_conditions":["thesis invalidated"]}"#.to_string())
            } else {
                Ok(r#"{"stance":"watch","confidence":0.6,"analysis":"bounded analysis","risks":["tail risk"],"evidence_gaps":["fresh data"],"challenges":["check assumptions"]}"#.to_string())
            }
        }

        fn provider(&self) -> String {
            "fake".to_string()
        }

        fn model(&self) -> String {
            "fake-model".to_string()
        }
    }

    #[tokio::test]
    async fn panel_runs_independent_cross_examination_and_manager_rounds() {
        let directory = tempfile::tempdir().expect("tempdir");
        let gateway = Arc::new(FakeGateway {
            calls: AtomicUsize::new(0),
        });
        let mut panel =
            ExpertPanel::with_gateway(directory.path().join("meetings.json"), gateway.clone());

        let meeting = panel
            .convene(
                ReviewScenario::TradeRisk,
                "Should exposure increase?",
                "Volatility is elevated.",
            )
            .await
            .expect("meeting");

        assert_eq!(gateway.calls.load(Ordering::Relaxed), 11);
        assert_eq!(meeting.contributions.len(), 10);
        assert_eq!(meeting.status, MeetingStatus::Complete);
        assert_eq!(meeting.conclusion.confidence, 0.72);
        assert!(meeting.contributions.iter().any(|item| item.round == 2));
    }

    #[test]
    fn invalid_expert_json_is_marked_failed_instead_of_becoming_advice() {
        let role = roles_for(ReviewScenario::InvestmentThesis).remove(0);
        let contribution = parse_contribution(&role, 1, "not json");
        assert!(contribution.failed);
        assert_eq!(contribution.confidence, 0.0);
    }
}
