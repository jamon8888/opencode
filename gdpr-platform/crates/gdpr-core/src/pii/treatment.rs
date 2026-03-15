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
