use std::collections::HashMap;
use once_cell::sync::Lazy;
use crate::pii::profile::AnonProfile;
use crate::pii::entity_category::EntityCategory;
use crate::pii::treatment::Treatment;

pub static PROFILES: Lazy<HashMap<AnonProfile, HashMap<EntityCategory, Treatment>>> = Lazy::new(|| {
    let mut map = HashMap::new();

    // MAX: anonymize everything
    map.insert(AnonProfile::Max, {
        let mut m = HashMap::new();
        m.insert(EntityCategory::Per,         Treatment::Mask);
        m.insert(EntityCategory::Org,         Treatment::Mask);
        m.insert(EntityCategory::OrgForm,     Treatment::Keep);
        m.insert(EntityCategory::OrgSector,   Treatment::Keep);
        m.insert(EntityCategory::OrgStruct,   Treatment::Keep);
        m.insert(EntityCategory::Role,        Treatment::Keep);
        m.insert(EntityCategory::Judge,       Treatment::Mask);
        m.insert(EntityCategory::Lawyer,      Treatment::Mask);
        m.insert(EntityCategory::Addr,        Treatment::Mask);
        m.insert(EntityCategory::AddrCity,    Treatment::Mask);
        m.insert(EntityCategory::AddrDept,    Treatment::Keep);
        m.insert(EntityCategory::AddrCountry, Treatment::Keep);
        m.insert(EntityCategory::DateAbs,     Treatment::Mask);
        m.insert(EntityCategory::DateRel,     Treatment::Keep);
        m.insert(EntityCategory::Amount,      Treatment::Mask);
        m.insert(EntityCategory::AmountRange, Treatment::Mask);
        m.insert(EntityCategory::Ratio,       Treatment::Keep);
        m.insert(EntityCategory::IdNat,       Treatment::Mask);
        m.insert(EntityCategory::IdFin,       Treatment::Mask);
        m.insert(EntityCategory::IdReg,       Treatment::Mask);
        m.insert(EntityCategory::IdProp,      Treatment::Mask);
        m.insert(EntityCategory::IdImmo,      Treatment::Mask);
        m.insert(EntityCategory::Contact,     Treatment::Mask);
        m.insert(EntityCategory::JurisRef,    Treatment::Keep);
        m.insert(EntityCategory::LawRef,      Treatment::Keep);
        m.insert(EntityCategory::ContractRef, Treatment::Mask);
        m.insert(EntityCategory::AssetDesc,   Treatment::Mask);
        m.insert(EntityCategory::Zone,        Treatment::Keep);
        m.insert(EntityCategory::Ccn,         Treatment::Mask);
        m.insert(EntityCategory::FinStruct,   Treatment::Mask);
        m
    });

    // DEAL: M&A — orgs pseudonymized, amounts generalized
    map.insert(AnonProfile::Deal, {
        let mut m = HashMap::new();
        m.insert(EntityCategory::Per,         Treatment::Pseudonym);
        m.insert(EntityCategory::Org,         Treatment::Pseudonym);
        m.insert(EntityCategory::OrgForm,     Treatment::Keep);
        m.insert(EntityCategory::OrgSector,   Treatment::Keep);
        m.insert(EntityCategory::OrgStruct,   Treatment::Keep);
        m.insert(EntityCategory::Role,        Treatment::Keep);
        m.insert(EntityCategory::Judge,       Treatment::Pseudonym);
        m.insert(EntityCategory::Lawyer,      Treatment::Pseudonym);
        m.insert(EntityCategory::Addr,        Treatment::Mask);
        m.insert(EntityCategory::AddrCity,    Treatment::Keep);
        m.insert(EntityCategory::AddrDept,    Treatment::Keep);
        m.insert(EntityCategory::AddrCountry, Treatment::Keep);
        m.insert(EntityCategory::DateAbs,     Treatment::Relativize);
        m.insert(EntityCategory::DateRel,     Treatment::Keep);
        m.insert(EntityCategory::Amount,      Treatment::Generalize);
        m.insert(EntityCategory::AmountRange, Treatment::Generalize);
        m.insert(EntityCategory::Ratio,       Treatment::Keep);
        m.insert(EntityCategory::IdNat,       Treatment::Mask);
        m.insert(EntityCategory::IdFin,       Treatment::Mask);
        m.insert(EntityCategory::IdReg,       Treatment::Mask);
        m.insert(EntityCategory::IdProp,      Treatment::Mask);
        m.insert(EntityCategory::IdImmo,      Treatment::Mask);
        m.insert(EntityCategory::Contact,     Treatment::Mask);
        m.insert(EntityCategory::JurisRef,    Treatment::Keep);
        m.insert(EntityCategory::LawRef,      Treatment::Keep);
        m.insert(EntityCategory::ContractRef, Treatment::Keep);
        m.insert(EntityCategory::AssetDesc,   Treatment::Generalize);
        m.insert(EntityCategory::Zone,        Treatment::Keep);
        m.insert(EntityCategory::Ccn,         Treatment::Mask);
        m.insert(EntityCategory::FinStruct,   Treatment::Keep);
        m
    });

    // LITIGE: litigation — parties pseudonymized, legal refs kept
    map.insert(AnonProfile::Litige, {
        let mut m = HashMap::new();
        m.insert(EntityCategory::Per,         Treatment::Pseudonym);
        m.insert(EntityCategory::Org,         Treatment::Pseudonym);
        m.insert(EntityCategory::OrgForm,     Treatment::Keep);
        m.insert(EntityCategory::OrgSector,   Treatment::Keep);
        m.insert(EntityCategory::OrgStruct,   Treatment::Keep);
        m.insert(EntityCategory::Role,        Treatment::Keep);
        m.insert(EntityCategory::Judge,       Treatment::Pseudonym);
        m.insert(EntityCategory::Lawyer,      Treatment::Pseudonym);
        m.insert(EntityCategory::Addr,        Treatment::Generalize);
        m.insert(EntityCategory::AddrCity,    Treatment::Keep);
        m.insert(EntityCategory::AddrDept,    Treatment::Keep);
        m.insert(EntityCategory::AddrCountry, Treatment::Keep);
        m.insert(EntityCategory::DateAbs,     Treatment::Relativize);
        m.insert(EntityCategory::DateRel,     Treatment::Keep);
        m.insert(EntityCategory::Amount,      Treatment::Generalize);
        m.insert(EntityCategory::AmountRange, Treatment::Generalize);
        m.insert(EntityCategory::Ratio,       Treatment::Keep);
        m.insert(EntityCategory::IdNat,       Treatment::Mask);
        m.insert(EntityCategory::IdFin,       Treatment::Mask);
        m.insert(EntityCategory::IdReg,       Treatment::Keep);
        m.insert(EntityCategory::IdProp,      Treatment::Keep);
        m.insert(EntityCategory::IdImmo,      Treatment::Keep);
        m.insert(EntityCategory::Contact,     Treatment::Mask);
        m.insert(EntityCategory::JurisRef,    Treatment::Keep);
        m.insert(EntityCategory::LawRef,      Treatment::Keep);
        m.insert(EntityCategory::ContractRef, Treatment::Keep);
        m.insert(EntityCategory::AssetDesc,   Treatment::Generalize);
        m.insert(EntityCategory::Zone,        Treatment::Keep);
        m.insert(EntityCategory::Ccn,         Treatment::Keep);
        m.insert(EntityCategory::FinStruct,   Treatment::Keep);
        m
    });

    // CONTRAT: contracts
    map.insert(AnonProfile::Contrat, {
        let mut m = HashMap::new();
        m.insert(EntityCategory::Per,         Treatment::Pseudonym);
        m.insert(EntityCategory::Org,         Treatment::Pseudonym);
        m.insert(EntityCategory::OrgForm,     Treatment::Keep);
        m.insert(EntityCategory::OrgSector,   Treatment::Keep);
        m.insert(EntityCategory::OrgStruct,   Treatment::Keep);
        m.insert(EntityCategory::Role,        Treatment::Keep);
        m.insert(EntityCategory::Judge,       Treatment::Mask);
        m.insert(EntityCategory::Lawyer,      Treatment::Pseudonym);
        m.insert(EntityCategory::Addr,        Treatment::Generalize);
        m.insert(EntityCategory::AddrCity,    Treatment::Keep);
        m.insert(EntityCategory::AddrDept,    Treatment::Keep);
        m.insert(EntityCategory::AddrCountry, Treatment::Keep);
        m.insert(EntityCategory::DateAbs,     Treatment::Relativize);
        m.insert(EntityCategory::DateRel,     Treatment::Keep);
        m.insert(EntityCategory::Amount,      Treatment::Generalize);
        m.insert(EntityCategory::AmountRange, Treatment::Generalize);
        m.insert(EntityCategory::Ratio,       Treatment::Keep);
        m.insert(EntityCategory::IdNat,       Treatment::Mask);
        m.insert(EntityCategory::IdFin,       Treatment::Mask);
        m.insert(EntityCategory::IdReg,       Treatment::Keep);
        m.insert(EntityCategory::IdProp,      Treatment::Mask);
        m.insert(EntityCategory::IdImmo,      Treatment::Mask);
        m.insert(EntityCategory::Contact,     Treatment::Mask);
        m.insert(EntityCategory::JurisRef,    Treatment::Keep);
        m.insert(EntityCategory::LawRef,      Treatment::Keep);
        m.insert(EntityCategory::ContractRef, Treatment::Keep);
        m.insert(EntityCategory::AssetDesc,   Treatment::Generalize);
        m.insert(EntityCategory::Zone,        Treatment::Keep);
        m.insert(EntityCategory::Ccn,         Treatment::Mask);
        m.insert(EntityCategory::FinStruct,   Treatment::Keep);
        m
    });

    // SOCIAL: labour law — CCN kept (CRITICAL)
    map.insert(AnonProfile::Social, {
        let mut m = HashMap::new();
        m.insert(EntityCategory::Per,         Treatment::Pseudonym);
        m.insert(EntityCategory::Org,         Treatment::Pseudonym);
        m.insert(EntityCategory::OrgForm,     Treatment::Keep);
        m.insert(EntityCategory::OrgSector,   Treatment::Keep);
        m.insert(EntityCategory::OrgStruct,   Treatment::Keep);
        m.insert(EntityCategory::Role,        Treatment::Keep);
        m.insert(EntityCategory::Judge,       Treatment::Pseudonym);
        m.insert(EntityCategory::Lawyer,      Treatment::Pseudonym);
        m.insert(EntityCategory::Addr,        Treatment::Mask);
        m.insert(EntityCategory::AddrCity,    Treatment::Keep);
        m.insert(EntityCategory::AddrDept,    Treatment::Keep);
        m.insert(EntityCategory::AddrCountry, Treatment::Keep);
        m.insert(EntityCategory::DateAbs,     Treatment::Relativize);
        m.insert(EntityCategory::DateRel,     Treatment::Keep);
        m.insert(EntityCategory::Amount,      Treatment::Generalize);
        m.insert(EntityCategory::AmountRange, Treatment::Generalize);
        m.insert(EntityCategory::Ratio,       Treatment::Keep);
        m.insert(EntityCategory::IdNat,       Treatment::Mask);
        m.insert(EntityCategory::IdFin,       Treatment::Mask);
        m.insert(EntityCategory::IdReg,       Treatment::Keep);
        m.insert(EntityCategory::IdProp,      Treatment::Mask);
        m.insert(EntityCategory::IdImmo,      Treatment::Mask);
        m.insert(EntityCategory::Contact,     Treatment::Mask);
        m.insert(EntityCategory::JurisRef,    Treatment::Keep);
        m.insert(EntityCategory::LawRef,      Treatment::Keep);
        m.insert(EntityCategory::ContractRef, Treatment::Keep);
        m.insert(EntityCategory::AssetDesc,   Treatment::Mask);
        m.insert(EntityCategory::Zone,        Treatment::Keep);
        m.insert(EntityCategory::Ccn,         Treatment::Keep); // CRITICAL: Social+Ccn = Keep
        m.insert(EntityCategory::FinStruct,   Treatment::Keep);
        m
    });

    // IMMO: real estate — AssetDesc kept (CRITICAL), addr kept
    map.insert(AnonProfile::Immo, {
        let mut m = HashMap::new();
        m.insert(EntityCategory::Per,         Treatment::Pseudonym);
        m.insert(EntityCategory::Org,         Treatment::Pseudonym);
        m.insert(EntityCategory::OrgForm,     Treatment::Keep);
        m.insert(EntityCategory::OrgSector,   Treatment::Keep);
        m.insert(EntityCategory::OrgStruct,   Treatment::Keep);
        m.insert(EntityCategory::Role,        Treatment::Keep);
        m.insert(EntityCategory::Judge,       Treatment::Mask);
        m.insert(EntityCategory::Lawyer,      Treatment::Pseudonym);
        m.insert(EntityCategory::Addr,        Treatment::Keep);
        m.insert(EntityCategory::AddrCity,    Treatment::Keep);
        m.insert(EntityCategory::AddrDept,    Treatment::Keep);
        m.insert(EntityCategory::AddrCountry, Treatment::Keep);
        m.insert(EntityCategory::DateAbs,     Treatment::Relativize);
        m.insert(EntityCategory::DateRel,     Treatment::Keep);
        m.insert(EntityCategory::Amount,      Treatment::Generalize);
        m.insert(EntityCategory::AmountRange, Treatment::Generalize);
        m.insert(EntityCategory::Ratio,       Treatment::Keep);
        m.insert(EntityCategory::IdNat,       Treatment::Mask);
        m.insert(EntityCategory::IdFin,       Treatment::Mask);
        m.insert(EntityCategory::IdReg,       Treatment::Keep);
        m.insert(EntityCategory::IdProp,      Treatment::Keep);
        m.insert(EntityCategory::IdImmo,      Treatment::Keep);
        m.insert(EntityCategory::Contact,     Treatment::Mask);
        m.insert(EntityCategory::JurisRef,    Treatment::Keep);
        m.insert(EntityCategory::LawRef,      Treatment::Keep);
        m.insert(EntityCategory::ContractRef, Treatment::Keep);
        m.insert(EntityCategory::AssetDesc,   Treatment::Keep); // CRITICAL: Immo+AssetDesc = Keep
        m.insert(EntityCategory::Zone,        Treatment::Keep);
        m.insert(EntityCategory::Ccn,         Treatment::Mask);
        m.insert(EntityCategory::FinStruct,   Treatment::Keep);
        m
    });

    // RESTRUCT: restructuring — DateAbs KEPT (CRITICAL, NOT Relativize)
    map.insert(AnonProfile::Restruct, {
        let mut m = HashMap::new();
        m.insert(EntityCategory::Per,         Treatment::Pseudonym);
        m.insert(EntityCategory::Org,         Treatment::Pseudonym);
        m.insert(EntityCategory::OrgForm,     Treatment::Keep);
        m.insert(EntityCategory::OrgSector,   Treatment::Keep);
        m.insert(EntityCategory::OrgStruct,   Treatment::Keep);
        m.insert(EntityCategory::Role,        Treatment::Keep);
        m.insert(EntityCategory::Judge,       Treatment::Pseudonym);
        m.insert(EntityCategory::Lawyer,      Treatment::Pseudonym);
        m.insert(EntityCategory::Addr,        Treatment::Mask);
        m.insert(EntityCategory::AddrCity,    Treatment::Keep);
        m.insert(EntityCategory::AddrDept,    Treatment::Keep);
        m.insert(EntityCategory::AddrCountry, Treatment::Keep);
        m.insert(EntityCategory::DateAbs,     Treatment::Keep); // CRITICAL: Restruct+DateAbs = Keep (NOT Relativize)
        m.insert(EntityCategory::DateRel,     Treatment::Keep);
        m.insert(EntityCategory::Amount,      Treatment::Generalize);
        m.insert(EntityCategory::AmountRange, Treatment::Generalize);
        m.insert(EntityCategory::Ratio,       Treatment::Keep);
        m.insert(EntityCategory::IdNat,       Treatment::Mask);
        m.insert(EntityCategory::IdFin,       Treatment::Mask);
        m.insert(EntityCategory::IdReg,       Treatment::Keep);
        m.insert(EntityCategory::IdProp,      Treatment::Keep);
        m.insert(EntityCategory::IdImmo,      Treatment::Mask);
        m.insert(EntityCategory::Contact,     Treatment::Mask);
        m.insert(EntityCategory::JurisRef,    Treatment::Keep);
        m.insert(EntityCategory::LawRef,      Treatment::Keep);
        m.insert(EntityCategory::ContractRef, Treatment::Keep);
        m.insert(EntityCategory::AssetDesc,   Treatment::Generalize);
        m.insert(EntityCategory::Zone,        Treatment::Keep);
        m.insert(EntityCategory::Ccn,         Treatment::Mask);
        m.insert(EntityCategory::FinStruct,   Treatment::Keep);
        m
    });

    map
});

pub fn get_treatment(profile: &AnonProfile, category: &EntityCategory) -> Treatment {
    if *profile == AnonProfile::Custom {
        return Treatment::Mask;
    }
    PROFILES
        .get(profile)
        .and_then(|m| m.get(category))
        .copied()
        .unwrap_or(Treatment::Mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_restruct_date_abs_is_keep() {
        assert_eq!(get_treatment(&AnonProfile::Restruct, &EntityCategory::DateAbs), Treatment::Keep);
    }

    #[test]
    fn test_deal_org_is_pseudonym() {
        assert_eq!(get_treatment(&AnonProfile::Deal, &EntityCategory::Org), Treatment::Pseudonym);
    }

    #[test]
    fn test_max_juris_ref_is_keep() {
        assert_eq!(get_treatment(&AnonProfile::Max, &EntityCategory::JurisRef), Treatment::Keep);
    }

    #[test]
    fn test_social_ccn_is_keep() {
        assert_eq!(get_treatment(&AnonProfile::Social, &EntityCategory::Ccn), Treatment::Keep);
    }

    #[test]
    fn test_immo_asset_desc_is_keep() {
        assert_eq!(get_treatment(&AnonProfile::Immo, &EntityCategory::AssetDesc), Treatment::Keep);
    }

    #[test]
    fn test_all_profiles_cover_all_categories() {
        use crate::pii::entity_category::EntityCategory;
        let all_profiles = [
            AnonProfile::Max, AnonProfile::Deal, AnonProfile::Litige,
            AnonProfile::Contrat, AnonProfile::Social, AnonProfile::Immo,
            AnonProfile::Restruct,
        ];
        let all_categories = EntityCategory::all_variants();
        for profile in &all_profiles {
            let matrix = PROFILES.get(profile).expect("profile must be in PROFILES");
            for category in &all_categories {
                assert!(matrix.contains_key(category), "Profile {:?} missing category {:?}", profile, category);
            }
        }
    }

    #[test]
    fn test_custom_profile_defaults_to_mask() {
        assert_eq!(get_treatment(&AnonProfile::Custom, &EntityCategory::Per), Treatment::Mask);
    }
}
