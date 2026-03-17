//! Prometheus metrics for the GDPR core library.

use prometheus::{Counter, CounterVec, GaugeVec, Histogram, HistogramOpts, Opts};
use std::sync::OnceLock;

pub struct Metrics {
    /// PII entities detected, broken down by detection layer.
    pub pii_detected: CounterVec,
    /// Documents successfully ingested (anonymized + stored).
    pub documents_ingested: Counter,
    /// Ingest operations where the NER/L2 layer was unavailable.
    pub ner_degraded: Counter,
    /// Documents erased under GDPR Art. 17 right-to-erasure.
    pub deletes: Counter,
    /// Wall-clock time of the PII anonymize call in milliseconds.
    pub detector_latency_ms: Histogram,
    /// Circuit breaker state per upstream service: `0.0` = closed, `1.0` = open.
    pub cb_state: GaugeVec,
}

static METRICS: OnceLock<Metrics> = OnceLock::new();

pub fn metrics() -> &'static Metrics {
    METRICS.get_or_init(|| {
        let pii_detected = CounterVec::new(
            Opts::new(
                "gdpr_pii_detected_total",
                "Total PII entities detected, by detection layer",
            ),
            &["detection_layer"],
        )
        .expect("gdpr_pii_detected_total: invalid metric spec");
        prometheus::register(Box::new(pii_detected.clone()))
            .expect("gdpr_pii_detected_total: register failed");

        let documents_ingested = Counter::with_opts(Opts::new(
            "gdpr_documents_ingested_total",
            "Total documents successfully ingested",
        ))
        .expect("gdpr_documents_ingested_total: invalid metric spec");
        prometheus::register(Box::new(documents_ingested.clone()))
            .expect("gdpr_documents_ingested_total: register failed");

        let ner_degraded = Counter::with_opts(Opts::new(
            "gdpr_ner_degraded_total",
            "Ingest operations where NER (L2) was unavailable",
        ))
        .expect("gdpr_ner_degraded_total: invalid metric spec");
        prometheus::register(Box::new(ner_degraded.clone()))
            .expect("gdpr_ner_degraded_total: register failed");

        let deletes = Counter::with_opts(Opts::new(
            "gdpr_delete_total",
            "Total documents erased (GDPR Art. 17 right to erasure)",
        ))
        .expect("gdpr_delete_total: invalid metric spec");
        prometheus::register(Box::new(deletes.clone()))
            .expect("gdpr_delete_total: register failed");

        let detector_latency_ms = Histogram::with_opts(
            HistogramOpts::new(
                "gdpr_detector_latency_ms",
                "PII anonymize call wall-clock latency in milliseconds",
            )
            .buckets(vec![
                1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 5000.0,
            ]),
        )
        .expect("gdpr_detector_latency_ms: invalid metric spec");
        prometheus::register(Box::new(detector_latency_ms.clone()))
            .expect("gdpr_detector_latency_ms: register failed");

        let cb_state = GaugeVec::new(
            Opts::new(
                "gdpr_cb_state",
                "Circuit breaker state per service: 0=closed, 1=open",
            ),
            &["service_name"],
        )
        .expect("gdpr_cb_state: invalid metric spec");
        prometheus::register(Box::new(cb_state.clone()))
            .expect("gdpr_cb_state: register failed");

        Metrics {
            pii_detected,
            documents_ingested,
            ner_degraded,
            deletes,
            detector_latency_ms,
            cb_state,
        }
    })
}

/// Encode all registered metrics as a Prometheus text exposition string.
pub fn gather_text() -> String {
    use prometheus::Encoder;
    let encoder = prometheus::TextEncoder::new();
    let mut buf = Vec::new();
    encoder.encode(&prometheus::gather(), &mut buf).unwrap_or_default();
    String::from_utf8(buf).unwrap_or_default()
}
