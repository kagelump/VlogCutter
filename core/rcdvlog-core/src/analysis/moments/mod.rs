//! Sparse, derived candidate moments for an asset.
//!
//! Coverage describes inspection performed by a recipe. Sampled regions are
//! sparse observations and must never be interpreted as continuous certification.

mod catalog;
mod logic;
mod model;
pub(crate) mod store;

pub use catalog::{
    MomentFailure, MomentInventoryItem, MomentInventoryPage, MomentPage, MomentPlan,
    MomentProgress, MomentRunReport,
};
pub use logic::{MomentPayload, MomentRunOptions};
pub use model::{
    CoverageLevel, CoverageSegment, EditProfile, MomentArtifact, MomentCandidate,
    MomentEvidenceRef, MomentProvenance, MomentReceipt, MomentSpan, MomentStatus,
};
pub use store::MomentStep;
