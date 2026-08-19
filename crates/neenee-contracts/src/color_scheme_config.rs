//! Pure-data custom color scheme and theme file schemas shared by persistence and frontends.

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

/// Component-specific override for the live prompt / input box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct InputThemeConfig {
    pub bg_active: Option<String>,
    pub bg_inactive: Option<String>,
    pub caret: Option<String>,
    pub selection: Option<String>,
    pub placeholder: Option<String>,
}

/// Component-specific override for crate tags and package badges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct CrateThemeConfig {
    pub fg: Option<String>,
    pub badge_bg: Option<String>,
}

/// Component-specific override for diff rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct DiffThemeConfig {
    pub add_bg: Option<String>,
    pub del_bg: Option<String>,
    pub add_hl: Option<String>,
    pub del_hl: Option<String>,
}

/// Component-specific override for command card rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct CommandThemeConfig {
    pub idle_bg: Option<String>,
    pub hover_bg: Option<String>,
}

/// Specialized component theme overrides container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(default)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct ComponentThemesConfig {
    pub input: Option<InputThemeConfig>,
    #[serde(rename = "crate")]
    pub crate_component: Option<CrateThemeConfig>,
    pub diff: Option<DiffThemeConfig>,
    pub command: Option<CommandThemeConfig>,
}

/// Full standalone theme file loaded from `$XDG_CONFIG_HOME/neenee/themes/<id>.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(default)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct ThemeFile {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: Option<String>,
    pub version: Option<String>,
    pub colors: ColorSchemeConfig,
    pub components: Option<ComponentThemesConfig>,
}

impl Default for ThemeFile {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: "Custom".to_string(),
            description: String::new(),
            author: None,
            version: None,
            colors: ColorSchemeConfig::default(),
            components: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_theme_file_with_components() {
        let raw = r##"
name = "Tokyo Night"
description = "Clean dark theme celebrating the lights of downtown Tokyo"
author = "folke"
version = "1.0.0"

[colors]
background = "#1a1b26"
surface = "#24283b"
text = "#c0caf5"
muted = "#565f89"
accent = "#7aa2f7"
success = "#9ece6a"
warning = "#e0af68"
error = "#f7768e"

[components.input]
bg_active = "#292e42"
bg_inactive = "#1f2335"
caret = "#c0caf5"

[components.crate]
fg = "#bb9af7"
badge_bg = "#283457"
"##;
        let parsed: ThemeFile = toml::from_str(raw).expect("theme file should parse");
        assert_eq!(parsed.name, "Tokyo Night");
        assert_eq!(parsed.colors.background, "#1a1b26");
        let components = parsed.components.expect("components should exist");
        let input = components.input.expect("input should exist");
        assert_eq!(input.bg_active.as_deref(), Some("#292e42"));
        assert_eq!(input.caret.as_deref(), Some("#c0caf5"));
        let crate_c = components.crate_component.expect("crate should exist");
        assert_eq!(crate_c.fg.as_deref(), Some("#bb9af7"));
    }
}

