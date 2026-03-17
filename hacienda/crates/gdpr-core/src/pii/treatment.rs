use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Treatment {
    Mask,
    Keep,
    Generalize,
    Pseudonym,
    Relativize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_treatment_serde_roundtrip() {
        let cases = [
            (Treatment::Mask, "\"mask\""),
            (Treatment::Keep, "\"keep\""),
            (Treatment::Generalize, "\"generalize\""),
            (Treatment::Pseudonym, "\"pseudonym\""),
            (Treatment::Relativize, "\"relativize\""),
        ];
        for (variant, expected_json) in &cases {
            let serialized = serde_json::to_string(variant).unwrap();
            assert_eq!(serialized, *expected_json, "serialize {:?}", variant);
            let deserialized: Treatment = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, *variant, "deserialize {}", expected_json);
        }
    }
}
