// Public items in this library crate are intentionally exposed for external
// consumers and integration tests. The binary target does not use them all,
// so the dead_code lint fires spuriously — suppress it at the crate level.
#![allow(dead_code, unused_imports)]

pub mod audit;
pub mod clients;
pub mod error;
pub mod extraction;
pub mod mcp;
pub mod ner;
pub mod pii;
pub mod resilience;
pub mod state;
