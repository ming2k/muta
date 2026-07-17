//! Durable, event-driven context added to an agent's live conversation window.
//!
//! These messages are distinct from request projection: lifecycle owners decide
//! when to append them, and the resulting provenance survives persistence.

mod messages;
mod skills;
mod system_reminder;

pub(crate) use messages::{hidden_user, hidden_user_with_reason, tool_image, visible_user};
pub(crate) use skills::inject_mentioned_skills;
pub(crate) use system_reminder::inject as inject_reminders;
