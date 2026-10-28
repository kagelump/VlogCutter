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
pub(crate) use logic::RECIPE_VERSION;
pub use logic::{MomentPayload, MomentRunOptions};
pub(crate) use logic::{
    PROMPT_VERSION as MOMENT_PROMPT_VERSION, RECIPE_VERSION as MOMENT_RECIPE_VERSION,
    SCHEMA_VERSION as MOMENT_SCHEMA_VERSION, output_schema as moment_output_schema, validate_reply,
};
pub use model::{
    CoverageLevel, CoverageSegment, EditProfile, MomentArtifact, MomentCandidate,
    MomentEvidenceRef, MomentProvenance, MomentReceipt, MomentSpan, MomentStatus,
    NativeMomentSource,
};
pub use store::MomentStep;
