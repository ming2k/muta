//! Full-screen view destinations (ADR-0141).
//!
//! A **view** is an independent, full-screen destination (`Session`, `Dashboard`, `Settings`, `Envoy`, `Side`).
//! Modals and overlays float over views and never own the full screen.

pub mod settings;

#[allow(unused_imports)]
pub use settings::{
    ConfigCategory, ConfigFocus, ConfigRects, ConfigViewProps, build_websearch_provider_dropdown,
    build_websearch_reader_dropdown, draw_settings_view,
};
