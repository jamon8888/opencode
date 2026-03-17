pub mod error;
pub mod state;
pub mod audit;
pub mod pii;
pub mod ner;
pub mod profiles;
pub mod clients;
pub mod resilience;
pub mod extraction;

// Re-export key types
pub use error::GdprError;
pub use state::CoreState;
