//! Periodic refresh adapter for a live [`SkillRegistry`].

use std::time::Duration;

use muta_contracts::DynamicCatalog;

use crate::SkillRegistry;

/// A dynamic catalog that periodically re-scans local and remote skill
/// sources while retaining the last successfully discovered registry state.
pub struct SkillCatalog {
    registry: SkillRegistry,
}

impl SkillCatalog {
    pub fn new(registry: SkillRegistry) -> Self {
        Self { registry }
    }
}

impl DynamicCatalog for SkillCatalog {
    fn id(&self) -> &'static str {
        "skills"
    }

    async fn refresh(&self) -> Result<(), String> {
        self.registry.reload().await;
        Ok(())
    }

    fn refresh_period(&self) -> Duration {
        Duration::from_secs(60 * 60)
    }
}
