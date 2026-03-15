use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProfileName {
    #[default]
    Standard,
    Medical,
    Financial,
    Legal,
    Full,
}

impl ProfileName {
    pub fn ner_labels(&self) -> &'static [&'static str] {
        match self {
            ProfileName::Standard  => &["PERSON", "EMAIL", "PHONE", "LOCATION", "ORG"],
            ProfileName::Medical   => &["PERSON", "EMAIL", "PHONE", "LOCATION", "ORG", "DATE", "MEDICAL_ID"],
            ProfileName::Financial => &["PERSON", "EMAIL", "PHONE", "LOCATION", "ORG", "IBAN", "CREDIT_CARD", "TAX_ID"],
            ProfileName::Legal     => &["PERSON", "EMAIL", "PHONE", "LOCATION", "ORG", "DATE", "CASE_ID", "CONTRACT_ID"],
            ProfileName::Full      => &["PERSON", "EMAIL", "PHONE", "LOCATION", "ORG", "DATE", "IBAN", "CREDIT_CARD", "TAX_ID", "MEDICAL_ID", "CASE_ID", "CONTRACT_ID", "IP_ADDRESS"],
        }
    }
}

pub struct ProfileInfo {
    pub name: ProfileName,
    pub label_count: usize,
}

pub struct ProfileRegistry;

impl ProfileRegistry {
    pub fn ner_labels(profile: ProfileName) -> &'static [&'static str] {
        profile.ner_labels()
    }

    pub fn list() -> Vec<ProfileInfo> {
        vec![
            ProfileInfo { name: ProfileName::Standard,  label_count: 5 },
            ProfileInfo { name: ProfileName::Medical,   label_count: 7 },
            ProfileInfo { name: ProfileName::Financial, label_count: 8 },
            ProfileInfo { name: ProfileName::Legal,     label_count: 8 },
            ProfileInfo { name: ProfileName::Full,      label_count: 13 },
        ]
    }
}
