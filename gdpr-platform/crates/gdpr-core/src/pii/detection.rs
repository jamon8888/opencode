use serde::{Deserialize, Serialize};

use super::entity_category::EntityCategory;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    pub value:      String,
    pub category:   EntityCategory,
    pub start:      usize,
    pub end:        usize,
    pub confidence: f32,
    pub layer:      DetectionLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionLayer {
    L1Regex,
    L1Ner,
    L2Ner,
    L3Strict,
}
