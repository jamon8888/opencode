use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EntityCategory {
    Per,
    Org,
    OrgForm,
    OrgSector,
    OrgStruct,
    Role,
    Judge,
    Lawyer,
    Addr,
    AddrCity,
    AddrDept,
    AddrCountry,
    DateAbs,
    DateRel,
    Amount,
    AmountRange,
    Ratio,
    IdNat,
    IdFin,
    IdReg,
    IdProp,
    IdImmo,
    Contact,
    JurisRef,
    LawRef,
    ContractRef,
    AssetDesc,
    Zone,
    Ccn,
    FinStruct,
}

impl EntityCategory {
    pub fn placeholder_base(&self) -> &'static str {
        match self {
            Self::Per        => "PERSON",
            Self::Org        => "ORG",
            Self::OrgForm    => "ORG_FORM",
            Self::OrgSector  => "SECTOR",
            Self::OrgStruct  => "STRUCT",
            Self::Role       => "ROLE",
            Self::Judge      => "JUDGE",
            Self::Lawyer     => "LAWYER",
            Self::Addr       => "ADDR",
            Self::AddrCity   => "CITY",
            Self::AddrDept   => "DEPT",
            Self::AddrCountry => "COUNTRY",
            Self::DateAbs    => "DATE",
            Self::DateRel    => "DATE_REL",
            Self::Amount     => "AMOUNT",
            Self::AmountRange => "RANGE",
            Self::Ratio      => "RATIO",
            Self::IdNat      => "ID_NAT",
            Self::IdFin      => "ID_FIN",
            Self::IdReg      => "ID_REG",
            Self::IdProp     => "ID_PROP",
            Self::IdImmo     => "ID_IMMO",
            Self::Contact    => "CONTACT",
            Self::JurisRef   => "JURIS_REF",
            Self::LawRef     => "LAW_REF",
            Self::ContractRef => "CONTRACT_REF",
            Self::AssetDesc  => "ASSET",
            Self::Zone       => "ZONE",
            Self::Ccn        => "CCN",
            Self::FinStruct  => "FIN",
        }
    }
}

impl EntityCategory {
    pub fn all_variants() -> Vec<EntityCategory> {
        vec![
            EntityCategory::Per, EntityCategory::Org, EntityCategory::OrgForm,
            EntityCategory::OrgSector, EntityCategory::OrgStruct, EntityCategory::Role,
            EntityCategory::Judge, EntityCategory::Lawyer,
            EntityCategory::Addr, EntityCategory::AddrCity, EntityCategory::AddrDept,
            EntityCategory::AddrCountry,
            EntityCategory::DateAbs, EntityCategory::DateRel,
            EntityCategory::Amount, EntityCategory::AmountRange, EntityCategory::Ratio,
            EntityCategory::IdNat, EntityCategory::IdFin, EntityCategory::IdReg,
            EntityCategory::IdProp, EntityCategory::IdImmo,
            EntityCategory::Contact,
            EntityCategory::JurisRef, EntityCategory::LawRef, EntityCategory::ContractRef,
            EntityCategory::AssetDesc, EntityCategory::Zone, EntityCategory::Ccn,
            EntityCategory::FinStruct,
        ]
    }
}

pub fn normalize_label(raw: &str) -> EntityCategory {
    match raw.to_uppercase().as_str() {
        "PERSON" | "PER" | "FULL_NAME" | "FIRST_NAME" | "LAST_NAME" => EntityCategory::Per,
        "ORGANIZATION" | "ORG" | "COMPANY_NAME" => EntityCategory::Org,
        "ORG_FORM" => EntityCategory::OrgForm,
        "ORG_SECTOR" => EntityCategory::OrgSector,
        "ORG_STRUCT" => EntityCategory::OrgStruct,
        "ROLE" | "JOB_TITLE" => EntityCategory::Role,
        "JUDGE" | "MAGISTRATE" => EntityCategory::Judge,
        "LAWYER" => EntityCategory::Lawyer,
        "ADDRESS" | "ADDR" | "POSTAL_CODE" => EntityCategory::Addr,
        "ADDR_CITY" | "CITY" | "GPE" | "LOC" => EntityCategory::AddrCity,
        "ADDR_DEPT" | "DEPT" => EntityCategory::AddrDept,
        "ADDR_COUNTRY" | "COUNTRY" => EntityCategory::AddrCountry,
        "DATE" | "DATE_ABS" | "DATE_OF_BIRTH" => EntityCategory::DateAbs,
        "DATE_REL" => EntityCategory::DateRel,
        "AMOUNT" | "MONEY" => EntityCategory::Amount,
        "AMOUNT_RANGE" => EntityCategory::AmountRange,
        "RATIO" | "PERCENT" => EntityCategory::Ratio,
        "ID_NAT" | "SSN" | "NIR" | "NI_NUMBER" | "RPPS" | "NIF" => EntityCategory::IdNat,
        "ID_FIN" | "IBAN" | "BIC" | "CREDIT_CARD" | "BANK_ACCOUNT" => EntityCategory::IdFin,
        "ID_REG" | "SIREN" | "SIRET" | "VAT_NUMBER" | "RCS_NUMBER" => EntityCategory::IdReg,
        "ID_PROP" | "PATENT_NUMBER" | "TRADEMARK_NUMBER" => EntityCategory::IdProp,
        "ID_IMMO" | "CADASTRAL_REF" => EntityCategory::IdImmo,
        "CONTACT" | "EMAIL" | "PHONE_NUMBER" | "PHONE" => EntityCategory::Contact,
        "JURIS_REF" | "CASE_REFERENCE" => EntityCategory::JurisRef,
        "LAW_REF" | "LAW_REFERENCE" => EntityCategory::LawRef,
        "CONTRACT_REF" | "CONTRACT_CLAUSE" | "CONTRACT_NUMBER" => EntityCategory::ContractRef,
        "ASSET_DESC" | "ASSET_DESCRIPTION" => EntityCategory::AssetDesc,
        "ZONE" | "REGULATORY_ZONE" => EntityCategory::Zone,
        "CCN" | "COLLECTIVE_AGREEMENT" => EntityCategory::Ccn,
        "FIN_STRUCT" | "FINANCIAL_DATA" => EntityCategory::FinStruct,
        _ => EntityCategory::Per,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_person_uppercase() {
        assert_eq!(normalize_label("PERSON"), EntityCategory::Per);
    }

    #[test]
    fn normalize_person_lowercase() {
        assert_eq!(normalize_label("person"), EntityCategory::Per);
    }

    #[test]
    fn normalize_iban_to_id_fin() {
        assert_eq!(normalize_label("IBAN"), EntityCategory::IdFin);
    }

    #[test]
    fn normalize_siren_to_id_reg() {
        assert_eq!(normalize_label("SIREN"), EntityCategory::IdReg);
    }

    #[test]
    fn normalize_unknown_falls_back_to_per() {
        assert_eq!(normalize_label("unknown_xyz"), EntityCategory::Per);
    }

    #[test]
    fn placeholder_base_per() {
        assert_eq!(EntityCategory::Per.placeholder_base(), "PERSON");
    }

    #[test]
    fn placeholder_base_amount() {
        assert_eq!(EntityCategory::Amount.placeholder_base(), "AMOUNT");
    }
}
