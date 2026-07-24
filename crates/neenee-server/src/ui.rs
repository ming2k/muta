//! The headless frontend's [`UiBridge`].
//!
//! The server has no terminal and no clipboard, so the one frontend-side
//! capability the slash-command dispatcher can request (`/export` →
//! clipboard) is reported as unavailable rather than silently dropped.

use neenee_transport::UiBridge;

/// Headless UI bridge: every UI-side capability is unavailable.
pub struct HeadlessUi;

#[async_trait::async_trait]
impl UiBridge for HeadlessUi {
    async fn copy_to_clipboard(
        &self,
        _text: &str,
    ) -> Result<neenee_transport::CopyOutcome, String> {
        Err("headless server: clipboard unavailable".into())
    }
}
