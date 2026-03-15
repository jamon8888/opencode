pub mod engine;
pub mod pool;
pub mod entity_category;
pub mod detection;
pub mod deduplication;
pub mod treatment;
pub mod profile;

pub use engine::{AnonymizeResult, PiiEngine};
pub use pool::EnginePool;
pub use entity_category::{EntityCategory, normalize_label};
pub use detection::{Detection, DetectionLayer};
pub use deduplication::deduplicate;
pub use treatment::Treatment;
pub use profile::AnonProfile;
