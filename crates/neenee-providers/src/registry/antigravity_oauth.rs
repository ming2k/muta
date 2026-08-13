//! The `antigravity-oauth` provider template: Google-native models served
//! via Google Antigravity OAuth subscription.

use super::google::{GOOGLE_BUILTIN_MODELS, MODELS};
use super::ProviderTemplateSpec;

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "antigravity-oauth",
    baselines: MODELS,
    protocol: "google",
    discovery: true,
    fitting: false,
    models: GOOGLE_BUILTIN_MODELS,
};
