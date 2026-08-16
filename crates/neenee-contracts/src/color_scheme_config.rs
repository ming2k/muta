//! Pure-data custom color scheme shared by persistence and frontends.

use serde::{Deserialize, Serialize};

/// User-editable semantic colors for a custom frontend palette.
///
/// Values use `#RRGGBB`. Frontends validate input before persisting it and
/// fall back to these defaults if an older hand-edited config contains an
/// invalid value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(default)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct ColorSchemeConfig {
    pub background: String,
    pub surface: String,
    pub text: String,
    pub muted: String,
    pub accent: String,
    pub success: String,
    pub warning: String,
    pub error: String,
}

impl Default for ColorSchemeConfig {
    fn default() -> Self {
        Self {
            background: "#070808".to_string(),
            surface: "#0e0f0f".to_string(),
            text: "#d5d5cd".to_string(),
            muted: "#777d75".to_string(),
            accent: "#8ea191".to_string(),
            success: "#759475".to_string(),
            warning: "#b5955d".to_string(),
            error: "#be6f68".to_string(),
        }
    }
}
