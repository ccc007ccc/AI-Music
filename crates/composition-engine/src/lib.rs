//! Creative intent, edit authorization, and deterministic proposal review.
//!
//! The renderable [`music_core::Project`] remains the authority for musical
//! events. This crate reviews a creative plan and its patch without embedding
//! subjective composition concepts into that event model.

mod arrangement;
mod model;
mod review;
mod session;

pub use arrangement::*;
pub use model::*;
pub use review::ProposalReviewer;
pub use session::{CompositionSessionError, CompositionSessions};
