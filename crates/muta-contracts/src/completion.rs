//! Backend-owned command catalog shared by every frontend.

use serde::{Deserialize, Serialize};

/// One concrete example for a slash command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct CommandExample {
    pub command: String,
    pub description: String,
}

/// A canonical slash command and all metadata needed for completion/help.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct CommandSpec {
    pub name: String,
    pub summary: String,
    pub description: String,
    #[serde(default)]
    pub usage: Vec<String>,
    #[serde(default)]
    pub examples: Vec<CommandExample>,
    #[serde(default)]
    pub intent_keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub category: Option<String>,
}

/// A non-command trigger that steers users to a canonical command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct CommandSuggestion {
    pub trigger: String,
    pub target: String,
    pub reason: String,
}

/// Accepted compatibility spelling and its canonical command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct CommandAlias {
    pub name: String,
    pub target: String,
}

/// Complete backend command vocabulary for one hosted project/session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct CommandCatalog {
    pub commands: Vec<CommandSpec>,
    /// Accepted compatibility aliases are dispatchable but not advertised as
    /// completion rows.
    #[serde(default)]
    pub aliases: Vec<CommandAlias>,
    #[serde(default)]
    pub suggestions: Vec<CommandSuggestion>,
}

impl CommandCatalog {
    pub fn find(&self, name: &str) -> Option<&CommandSpec> {
        self.commands
            .iter()
            .find(|command| command.name == name)
            .or_else(|| {
                let target = self
                    .aliases
                    .iter()
                    .find(|alias| alias.name == name)
                    .map(|alias| alias.target.as_str())?;
                self.commands.iter().find(|command| command.name == target)
            })
    }

    pub fn recognizes(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    pub fn alias(&self, name: &str) -> Option<&CommandAlias> {
        self.aliases.iter().find(|alias| alias.name == name)
    }
}

/// Semantic kind of one backend-produced composer completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum InputCompletionKind {
    Slash,
    Intent,
    PathFile,
    PathDir,
    PathExplicit,
}

/// One completion edit produced by the daemon.
///
/// Replacement offsets are Unicode-scalar indices, not UTF-8 byte offsets or
/// JavaScript UTF-16 code units. That gives every frontend one stable wire
/// representation; clients translate to their native string indexing only at
/// the final edit boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct InputCompletion {
    /// Text shown in the completion menu.
    pub label: String,
    pub description: String,
    /// Exact text the client splices over `replace_start..replace_end`.
    /// This is separate from `label` so backend-owned edit behavior (for
    /// example consuming an `@` trigger or appending a trailing space) does
    /// not leak back into frontend code.
    pub insert_text: String,
    pub replace_start: usize,
    pub replace_end: usize,
    pub kind: InputCompletionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<CommandSpec>,
}
