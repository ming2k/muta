//! Network intelligence and structured multi-expert deliberation.
//!
//! This crate deliberately stays separate from `neenee-quant`: public-web
//! monitoring and expert meetings are useful to more than trading, while the
//! quant GUI can compose both application services without pushing either
//! concern into the presentation layer.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod opinion;
pub mod panel;

pub use opinion::{LinkChange, OpinionHub, OpinionItem, OpinionState, OpinionTopic, WatchedLink};
pub use panel::{
    ExpertContribution, ExpertMeeting, ExpertPanel, MeetingConclusion, MeetingStatus,
    ReviewScenario,
};
