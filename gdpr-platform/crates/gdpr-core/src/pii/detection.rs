use serde::{Deserialize, Serialize};

use super::entity_category::EntityCategory;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detection_layer_serde_roundtrip() {
        let cases = [
            (DetectionLayer::L1Regex, "\"l1_regex\""),
            (DetectionLayer::L1Ner, "\"l1_ner\""),
            (DetectionLayer::L2Ner, "\"l2_ner\""),
            (DetectionLayer::L3Strict, "\"l3_strict\""),
        ];
        for (variant, expected_json) in &cases {
            let serialized = serde_json::to_string(variant).unwrap();
            assert_eq!(serialized, *expected_json, "serialize {:?}", variant);
            let deserialized: DetectionLayer = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, *variant, "deserialize {}", expected_json);
        }
    }

    #[test]
    fn test_detection_partial_eq() {
        use crate::pii::entity_category::EntityCategory;
        let d1 = Detection {
            value: "Alice".to_string(),
            category: EntityCategory::Per,
            start: 0,
            end: 5,
            confidence: 0.9,
            layer: DetectionLayer::L1Regex,
        };
        let d2 = d1.clone();
        assert_eq!(d1, d2);
    }
}
