use std::collections::HashMap;
use once_cell::sync::Lazy;
use crate::pii::profile::AnonProfile;

pub static PSEUDO_POOLS: Lazy<HashMap<AnonProfile, Vec<&'static str>>> = Lazy::new(|| {
    let mut map = HashMap::new();
    map.insert(AnonProfile::Max, vec![
        "Entity Alpha", "Entity Beta", "Entity Gamma", "Entity Delta",
        "Entity Epsilon", "Entity Zeta", "Entity Eta", "Entity Theta",
    ]);
    map.insert(AnonProfile::Deal, vec![
        "Company Alpha", "Company Beta", "Company Gamma", "Company Delta",
        "Company Epsilon", "Company Zeta", "Company Eta", "Company Theta",
    ]);
    map.insert(AnonProfile::Litige, vec![
        "Party A", "Party B", "Party C", "Party D",
    ]);
    map.insert(AnonProfile::Contrat, vec![
        "Supplier", "Client", "Provider Alpha", "Provider Beta",
        "Counterparty A", "Counterparty B",
    ]);
    map.insert(AnonProfile::Social, vec![
        "Employer Alpha", "Employer Beta", "Employee A", "Employee B",
        "Union Representative", "HR Entity",
    ]);
    map.insert(AnonProfile::Immo, vec![
        "Landlord Alpha", "Tenant A", "Tenant B", "Property Manager",
        "SCI Alpha", "Investor Alpha",
    ]);
    map.insert(AnonProfile::Restruct, vec![
        "Holding Alpha", "Holding Beta", "Subsidiary A", "Subsidiary B",
        "Creditor Alpha", "Creditor Beta", "Advisor Alpha",
    ]);
    map
});

pub fn get_pool(profile: &AnonProfile) -> &'static [&'static str] {
    PSEUDO_POOLS.get(profile)
        .or_else(|| PSEUDO_POOLS.get(&AnonProfile::Max))
        .map(|v| v.as_slice())
        .unwrap_or(&["Entity Alpha", "Entity Beta"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deal_pool_not_empty() {
        let pool = get_pool(&AnonProfile::Deal);
        assert!(!pool.is_empty());
        assert!(pool.contains(&"Company Alpha"));
    }

    #[test]
    fn test_custom_falls_back_to_max() {
        let pool = get_pool(&AnonProfile::Custom);
        assert!(pool.contains(&"Entity Alpha"));
    }

    #[test]
    fn test_all_non_custom_profiles_have_pools() {
        for profile in [AnonProfile::Max, AnonProfile::Deal, AnonProfile::Litige,
                        AnonProfile::Contrat, AnonProfile::Social, AnonProfile::Immo,
                        AnonProfile::Restruct] {
            let pool = PSEUDO_POOLS.get(&profile);
            assert!(pool.is_some(), "Missing pool for {:?}", profile);
            assert!(!pool.unwrap().is_empty());
        }
    }
}
