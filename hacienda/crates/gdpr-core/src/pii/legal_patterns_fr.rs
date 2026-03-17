use once_cell::sync::Lazy;
use regex::Regex;
use crate::pii::{Detection, DetectionLayer, EntityCategory};

static RE_ROLE: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b(Président(?:e)?|Directeur\s+Général(?:e)?|DG|PDG|Gérant(?:e)?|Associé(?:e)?|Mandataire|Représentant(?:e)?)\b"
).unwrap());

static RE_JUDGE: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b(Monsieur|Madame|M\.|Mme\.?)\s+le\s+[Jj]uge\b|\bConseil(?:ler)?\s+(?:rapporteur|instructeur)\b"
).unwrap());

static RE_LAWYER: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\bMa[îi]tre\s+[A-ZÉÈÊËÀÂÙÛÜ][a-zéèêëàâùûü\-]+(?:\s+[A-ZÉÈÊËÀÂÙÛÜ][a-zéèêëàâùûü\-]+)*\b"
).unwrap());

static RE_LAW_REF: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b(?:Art(?:icle)?\.?\s+[LRD]\.?\s*\d+(?:[-.]\d+)*|L\.?\s*\d+(?:[-.]\d+)+\s+(?:du\s+)?[Cc]ode\b)"
).unwrap());

static RE_CCN: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\bCCN\s+[A-ZÀ-Ü][a-zà-ü\s]{3,50}|\bconvention\s+collective\s+(?:nationale\s+)?(?:de\s+)?[A-ZÀ-Ü][a-zà-ü\s]{3,50}"
).unwrap());

static RE_AMOUNT: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b\d[\d\s]*(?:[.,]\d+)?\s*(?:€|EUR|K€|M€|milliard(?:s)?\s*d'euros?|million(?:s)?\s*d'euros?)"
).unwrap());

static RE_AMOUNT_RANGE: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b(?:entre|de)\s+\d[\d\s]*(?:[.,]\d+)?\s*(?:€|EUR|K€|M€)\s+(?:et|à)\s+\d[\d\s]*(?:[.,]\d+)?\s*(?:€|EUR|K€|M€)"
).unwrap());

static RE_RATIO: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b\d+(?:[.,]\d+)?(?:x\s*EBITDA|%\s*(?:du\s+)?(?:capital|chiffre\s+d'affaires?|CA)|fois\s+le\s+résultat)\b"
).unwrap());

static RE_DATE_REL: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b(?:J[+\-]\d+|dans\s+\d+\s+(?:jours?|semaines?|mois|ans?)|sous\s+\d+\s+(?:jours?|semaines?|mois)|délai\s+de\s+\d+\s+(?:jours?|semaines?|mois|ans?))\b"
).unwrap());

static RE_ID_PROP: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b(?:brevet(?:\s+d'invention)?|EP\s*\d{5,7}|FR\s*\d{6,10}|marque\s+(?:déposée|enregistrée)\s*(?:n°\s*)?\d+)\b"
).unwrap());

static RE_ID_IMMO: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b(?:parcelle\s+(?:cadastrale\s*)?(?:n°\s*)?\w+|lot\s+(?:cadastral\s*)?(?:n°\s*)?\w+|section\s+cadastrale\s+[A-Z]+\s*\d+)\b"
).unwrap());

static RE_ZONE: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b(?:zone\s+(?:PLU\s*)?U[A-Z]?\b|ICPE\s+(?:rubrique\s+)?\d+|Natura\s+2000|zone\s+(?:naturelle|agricole|à\s+urbaniser|urbaine))\b"
).unwrap());

static RE_FIN_STRUCT: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"(?i)\b(?:chiffre\s+d'affaires?|CA\b|EBITDA|capitaux\s+propres|résultat\s+(?:net|d'exploitation|opérationnel)|endettement\s+(?:net|brut)|levier\s+(?:financier)?)\b"
).unwrap());

static RE_ORG_FORM: Lazy<Regex> = Lazy::new(|| Regex::new(
    r"\b(?:SAS|SARL|SA\b|SCI\b|SNC|SASU|EURL|GIE|SC\b|SCOP|SE\b|SLP|SELAFA|SELARL|SELAS)\b"
).unwrap());

pub fn detect_legal_patterns_fr(text: &str) -> Vec<Detection> {
    let mut detections = Vec::new();
    let rules: &[(&Lazy<Regex>, EntityCategory)] = &[
        (&RE_ROLE,        EntityCategory::Role),
        (&RE_JUDGE,       EntityCategory::Judge),
        (&RE_LAWYER,      EntityCategory::Lawyer),
        (&RE_LAW_REF,     EntityCategory::LawRef),
        (&RE_CCN,         EntityCategory::Ccn),
        (&RE_AMOUNT,      EntityCategory::Amount),
        (&RE_AMOUNT_RANGE,EntityCategory::AmountRange),
        (&RE_RATIO,       EntityCategory::Ratio),
        (&RE_DATE_REL,    EntityCategory::DateRel),
        (&RE_ID_PROP,     EntityCategory::IdProp),
        (&RE_ID_IMMO,     EntityCategory::IdImmo),
        (&RE_ZONE,        EntityCategory::Zone),
        (&RE_FIN_STRUCT,  EntityCategory::FinStruct),
        (&RE_ORG_FORM,    EntityCategory::OrgForm),
    ];
    for (pattern, category) in rules {
        for m in pattern.find_iter(text) {
            detections.push(Detection {
                value: m.as_str().to_string(),
                category: *category,
                start: m.start(),
                end: m.end(),
                confidence: 0.85,
                layer: DetectionLayer::L1Regex,
            });
        }
    }
    detections
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(d: &[Detection], cat: EntityCategory) -> bool {
        d.iter().any(|x| x.category == cat)
    }

    #[test]
    fn test_detects_lawyer() {
        let d = detect_legal_patterns_fr("Maître Dupont a signé.");
        assert!(has(&d, EntityCategory::Lawyer));
    }

    #[test]
    fn test_detects_ccn() {
        let d = detect_legal_patterns_fr("couverts par la CCN Syntec applicable.");
        assert!(has(&d, EntityCategory::Ccn));
    }

    #[test]
    fn test_detects_law_ref() {
        let d = detect_legal_patterns_fr("Conformément à l'Art. L.1237-19 du Code.");
        assert!(has(&d, EntityCategory::LawRef));
    }

    #[test]
    fn test_detects_amount() {
        let d = detect_legal_patterns_fr("fixé à 1 500 000 €.");
        assert!(has(&d, EntityCategory::Amount));
    }

    #[test]
    fn test_detects_date_rel() {
        let d = detect_legal_patterns_fr("paiement à J+30 après signature.");
        assert!(has(&d, EntityCategory::DateRel));
    }

    #[test]
    fn test_detects_org_form() {
        let d = detect_legal_patterns_fr("La société SAS Holding Alpha.");
        assert!(has(&d, EntityCategory::OrgForm));
    }

    #[test]
    fn test_empty_returns_empty() {
        assert!(detect_legal_patterns_fr("").is_empty());
    }
}
