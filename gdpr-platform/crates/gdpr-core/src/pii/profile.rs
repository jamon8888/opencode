use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AnonProfile {
    #[default]
    Max,
    Deal,
    Litige,
    Contrat,
    Social,
    Immo,
    Restruct,
    Custom,
}

impl AnonProfile {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Max      => "max",
            Self::Deal     => "deal",
            Self::Litige   => "litige",
            Self::Contrat  => "contrat",
            Self::Social   => "social",
            Self::Immo     => "immo",
            Self::Restruct => "restruct",
            Self::Custom   => "custom",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Max      => "Maximum masking — all PII masked",
            Self::Deal     => "M&A / Private Equity — organizations pseudonymized, amounts generalized",
            Self::Litige   => "Litigation / Arbitration — legal references kept, parties masked",
            Self::Contrat  => "Commercial contracts — organizations pseudonymized, contract references kept",
            Self::Social   => "Labour law / HR — CCN kept, organizations pseudonymized",
            Self::Immo     => "Real estate — asset descriptions kept, addresses generalized",
            Self::Restruct => "Restructuring / Collective procedures — dates exact (CRITICAL), financial structure kept",
            Self::Custom   => "Custom profile — user-defined treatment matrix",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_max() {
        assert_eq!(AnonProfile::default(), AnonProfile::Max);
    }
}
