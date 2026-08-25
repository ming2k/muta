//! Skill discovery, metadata, registries, and model-facing tool adapters.
//!
//! Skills are markdown files with YAML frontmatter, stored (in priority order,
//! lowest first) across:
//!   - Remote skill repositories fetched into `$XDG_CACHE_HOME/muta/skills/remote/`.
//!   - User-global skills: `$XDG_DATA_HOME/muta/skills/` (XDG-resolved via
//!     [`muta_persistence::paths`]).
//!   - External user-global formats: `~/.agents/skills/`, `~/.claude/skills/`
//!     (someone else's convention).
//!   - Configured extra paths (`[skills] paths = [...]` in `config.toml`).
//!   - Project-local skills: `.muta/skills/<name>/SKILL.md` (highest priority).
//!
//! Frontmatter schema:
//!   ```yaml
//!   ---
//!   name: rust-expert
//!   description: "Use when writing or debugging Rust code"
//!   short-description: "Rust help"
//!   version: "1.0.0"
//!   tags: [rust, cargo]
//!   policy:
//!     allow_implicit_invocation: true
//!   dependencies:
//!     - type: mcp
//!       value: context7
//!   ---
//!   ```

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod catalog;
pub mod discovery;
pub mod metadata;
pub mod remote;
pub mod render;
pub mod tools;

pub use catalog::SkillCatalog;
pub use discovery::ShadowedSkill;
pub use metadata::{Skill, SkillDependency, SkillPolicy, SkillScope};
pub use muta_contracts::SkillsConfig;
pub use render::{format_skills_for_prompt, resolve_mentions};
pub use tools::{ListSkillsTool, UseSkillTool};

use discovery::{DiscoveryResult, discover_all};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

/// Consumer for a scan's newly-observed shadowing events (a project-local
/// skill overriding a same-named lower-scope skill). The runtime installs one
/// to surface a warning notice; the crate itself stays UI-agnostic.
pub type ShadowSink = Arc<dyn Fn(&[ShadowedSkill]) + Send + Sync>;

/// Shadow-notice plumbing shared by every clone of a [`SkillRegistry`]:
/// the installed sink plus the set of skill names already reported this
/// process lifetime (dedupe — a name warns once, not on every hourly rescan).
#[derive(Default)]
struct ShadowState {
    sink: Option<ShadowSink>,
    reported: HashSet<String>,
}

/// Thread-safe in-memory registry of discovered skills.
#[derive(Clone)]
pub struct SkillRegistry {
    inner: Arc<RwLock<RegistryInner>>,
    /// Lazily-populated cache of skill bodies, keyed by skill name. A body is
    /// read from disk (via [`Skill::load_body`]) the first time it is
    /// requested, then reused so repeated `use_skill` / implicit loads in the
    /// same session never re-read the file.
    bodies: Arc<RwLock<HashMap<String, String>>>,
    shadows: Arc<Mutex<ShadowState>>,
}

#[derive(Debug, Default, Clone)]
struct RegistryInner {
    skills: Vec<Skill>,
    errors: Vec<String>,
    /// Project-local skills that shadowed a same-named lower-scope skill in
    /// the most recent scan (see [`discovery::ShadowedSkill`]).
    shadowed: Vec<ShadowedSkill>,
    config: SkillsConfig,
}

impl SkillRegistry {
    /// Create an empty registry with no configuration. `reload()` on such a
    /// registry re-runs discovery with a default (empty) config, so prefer
    /// [`SkillRegistry::empty_with_config`] when the real config is known.
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(RwLock::new(RegistryInner::default())),
            bodies: Arc::new(RwLock::new(HashMap::new())),
            shadows: Arc::new(Mutex::new(ShadowState::default())),
        }
    }

    /// Create an empty registry that remembers `config`. The registry starts
    /// with no discovered skills, but a subsequent [`reload`](Self::reload)
    /// (e.g. from the background refresh loop) re-runs discovery using this
    /// config and populates the registry in place. This is the entry point
    /// for non-blocking startup: hand back an empty registry immediately, let
    /// the background task fill it.
    pub fn empty_with_config(config: &SkillsConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(RegistryInner {
                skills: Vec::new(),
                errors: Vec::new(),
                shadowed: Vec::new(),
                config: config.clone(),
            })),
            bodies: Arc::new(RwLock::new(HashMap::new())),
            shadows: Arc::new(Mutex::new(ShadowState::default())),
        }
    }

    /// Discover skills from all configured sources.
    pub async fn load(config: &SkillsConfig) -> Self {
        let result = discover_all(config).await;
        if !result.errors.is_empty() {
            for err in &result.errors {
                tracing::warn!("skill discovery error: {}", err);
            }
        }
        let registry = Self {
            inner: Arc::new(RwLock::new(RegistryInner {
                skills: Vec::new(),
                errors: Vec::new(),
                shadowed: Vec::new(),
                config: config.clone(),
            })),
            bodies: Arc::new(RwLock::new(HashMap::new())),
            shadows: Arc::new(Mutex::new(ShadowState::default())),
        };
        registry.apply_scan(result);
        registry
    }

    /// Rescan all sources using the same configuration that was originally
    /// supplied. If no configuration was stored, performs a default scan.
    pub async fn reload(&self) {
        let config = {
            match self.inner.read() {
                Ok(inner) => inner.config.clone(),
                Err(err) => err.into_inner().config.clone(),
            }
        };
        let result = discover_all(&config).await;
        self.apply_scan(result);
    }

    /// Install (or clear) the shadow-notice sink. The sink is called after a
    /// scan with the shadowing events not yet reported this process lifetime
    /// — one call batch per scan, each name reported at most once ever.
    pub fn set_shadow_sink(&self, sink: Option<ShadowSink>) {
        if let Ok(mut shadows) = self.shadows.lock() {
            shadows.sink = sink;
        }
    }

    /// The most recent scan's shadowing events (project-local skills that
    /// overrode a same-named lower-scope skill), reported or not.
    pub fn shadowed(&self) -> Vec<ShadowedSkill> {
        self.lock().guard.shadowed.clone()
    }

    /// Store a finished scan and notify the shadow sink about newly-seen
    /// shadowing. Dedupe is by skill name across the registry's lifetime, so
    /// the hourly background rescan cannot spam a notice the user already saw.
    fn apply_scan(&self, result: DiscoveryResult) {
        if let Ok(mut inner) = self.inner.write() {
            inner.skills = result.skills;
            inner.errors = result.errors;
            inner.shadowed = result.shadowed.clone();
        }
        // Collect the not-yet-reported shadow records and mark them reported,
        // releasing the lock BEFORE invoking the (foreign) sink.
        let notify = self.shadows.lock().ok().and_then(|mut shadows| {
            let fresh: Vec<ShadowedSkill> = result
                .shadowed
                .into_iter()
                .filter(|s| !shadows.reported.contains(&s.name))
                .collect();
            for s in &fresh {
                shadows.reported.insert(s.name.clone());
            }
            match shadows.sink.clone() {
                Some(sink) if !fresh.is_empty() => Some((sink, fresh)),
                _ => None,
            }
        });
        if let Some((sink, fresh)) = notify {
            sink(&fresh);
        }
    }

    /// Acquire a read lock on the registry.
    pub fn lock(&self) -> RegistryGuard<'_> {
        RegistryGuard {
            guard: self.inner.read().unwrap_or_else(|e| e.into_inner()),
        }
    }

    /// Replace the registry contents directly, used during tests or when the
    /// caller wants to build a registry without disk discovery.
    pub fn replace(&self, skills: Vec<Skill>) {
        if let Ok(mut inner) = self.inner.write() {
            inner.skills = skills;
            inner.errors.clear();
            inner.shadowed.clear();
        }
        if let Ok(mut bodies) = self.bodies.write() {
            bodies.clear();
        }
    }

    /// Resolve a skill's body by name, loading it from disk on first access
    /// and caching the result for the lifetime of this registry.
    ///
    /// Returns `None` when no skill with that name is registered, and an
    /// `Err` only if the body genuinely cannot be read.
    pub fn body_for(&self, name: &str) -> Option<Result<String, String>> {
        let skill = self.lock().get(name)?;
        if let Ok(bodies) = self.bodies.read()
            && let Some(cached) = bodies.get(name)
        {
            return Some(Ok(cached.clone()));
        }
        let body = skill.load_body();
        if let Ok(ref text) = body
            && let Ok(mut bodies) = self.bodies.write()
        {
            bodies.insert(name.to_string(), text.clone());
        }
        Some(body)
    }
}

/// Read guard exposing registry contents.
pub struct RegistryGuard<'a> {
    guard: std::sync::RwLockReadGuard<'a, RegistryInner>,
}

impl RegistryGuard<'_> {
    pub fn get(&self, name: &str) -> Option<Skill> {
        self.guard.skills.iter().find(|s| s.name == name).cloned()
    }

    pub fn list(&self) -> Vec<Skill> {
        self.guard.skills.clone()
    }

    pub fn resolve_mentions(&self, text: &str) -> Vec<Skill> {
        render::resolve_mentions(text, &self.guard.skills)
            .into_iter()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shadow(name: &str) -> ShadowedSkill {
        ShadowedSkill {
            name: name.to_string(),
            overridden_scope: SkillScope::User,
            winner_source: std::path::PathBuf::from(format!("/proj/.muta/skills/{name}/SKILL.md")),
        }
    }

    fn scan_result(shadowed: Vec<ShadowedSkill>) -> DiscoveryResult {
        DiscoveryResult {
            skills: Vec::new(),
            errors: Vec::new(),
            shadowed,
        }
    }

    #[test]
    fn shadow_sink_fires_exactly_once_per_name() {
        let registry = SkillRegistry::empty();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_sink = Arc::clone(&seen);
        registry.set_shadow_sink(Some(Arc::new(move |batch: &[ShadowedSkill]| {
            seen_in_sink
                .lock()
                .unwrap()
                .extend(batch.iter().map(|s| s.name.clone()));
        })));

        // First scan reporting a shadow: the sink fires with just that name.
        registry.apply_scan(scan_result(vec![shadow("shared")]));
        assert_eq!(*seen.lock().unwrap(), vec!["shared".to_string()]);

        // A rescan reporting the SAME shadow (the hourly refresh finds the
        // override unchanged) must NOT re-notify — one warning per name.
        registry.apply_scan(scan_result(vec![shadow("shared")]));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["shared".to_string()],
            "repeat shadow of the same name is deduped"
        );

        // A scan that adds a second shadowed name reports only the new one.
        registry.apply_scan(scan_result(vec![shadow("shared"), shadow("other")]));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["shared".to_string(), "other".to_string()]
        );

        // The last scan's shadow set remains inspectable regardless of dedupe.
        assert_eq!(registry.shadowed().len(), 2);
    }

    #[test]
    fn shadow_sink_not_installed_means_no_panic_and_records_kept() {
        let registry = SkillRegistry::empty();
        registry.apply_scan(scan_result(vec![shadow("x")]));
        assert_eq!(registry.shadowed().len(), 1);
        // A sink installed LATER does not replay already-reported names.
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_sink = Arc::clone(&seen);
        registry.set_shadow_sink(Some(Arc::new(move |batch: &[ShadowedSkill]| {
            seen_in_sink
                .lock()
                .unwrap()
                .extend(batch.iter().map(|s| s.name.clone()));
        })));
        registry.apply_scan(scan_result(vec![shadow("x")]));
        assert!(seen.lock().unwrap().is_empty());
    }
}
