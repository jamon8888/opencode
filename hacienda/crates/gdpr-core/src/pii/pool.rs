//! EnginePool: eliminates `Arc<Mutex<PiiEngine>>` single-lock bottleneck on the proxy path.
//!
//! OPT-2: Round-robin counter for slot selection avoids always hitting slot 0.
//! Each slot has its own `Detector` (stateless, cheap clone).
//! `Vault` and `GlinerNer` are `Arc`-shared — pseudonym consistency and one ONNX session.
//!
//! **MUST** call `anonymize_batch` via `spawn_blocking` — L1 regex + L2 ONNX is CPU-bound.

use std::sync::atomic::{AtomicUsize, Ordering};
use crate::pii::engine::{AnonymizeResult, PiiEngine};

struct EngineSlot {
    engine: parking_lot::Mutex<PiiEngine>,
}

/// OPT-2: Round-robin pool with atomic counter for even slot distribution.
pub struct EnginePool {
    slots:   Vec<EngineSlot>,
    counter: AtomicUsize,
}

impl EnginePool {
    /// Build `size` independent slots from a seed via `try_clone`.
    /// All slots share the same `Vault` (pseudonym consistency) and `GlinerNer` (one ONNX session).
    pub fn new(size: usize, seed: PiiEngine) -> anyhow::Result<Self> {
        let size  = size.max(1);
        let mut slots = Vec::with_capacity(size);
        for _ in 0..size {
            slots.push(EngineSlot {
                engine: parking_lot::Mutex::new(seed.try_clone()?),
            });
        }
        Ok(Self { slots, counter: AtomicUsize::new(0) })
    }

    /// Anonymize a batch of texts, returning one result per input.
    ///
    /// OPT-2: Uses round-robin counter to pick the starting slot, then tries
    /// remaining slots with `try_lock`. Falls back to blocking lock on start slot.
    ///
    /// C7 fix: `ner_degraded` is per-element (`Vec<bool>`).
    ///
    /// On L1 detection failure (Invariant I1): returns error sentinel strings
    /// rather than passing raw content through.
    pub fn anonymize_batch(
        &self,
        texts: &[&str],
        ner_degraded: &mut Vec<bool>,
    ) -> anyhow::Result<Vec<AnonymizeResult>> {
        let n     = self.slots.len();
        let start = self.counter.fetch_add(1, Ordering::Relaxed) % n;
        for i in 0..n {
            let idx = (start + i) % n;
            if let Some(mut engine) = self.slots[idx].engine.try_lock() {
                return engine.anonymize_batch(texts, ner_degraded);
            }
        }
        // All slots busy — block on start slot rather than reject
        let mut engine = self.slots[start].engine.lock();
        engine.anonymize_batch(texts, ner_degraded)
    }

    /// Anonymize a batch of texts, returning cleaned strings (proxy path helper).
    ///
    /// On L1 detection failure (Invariant I1): returns `"[PII_DETECTION_ERROR: content blocked]"`
    /// for every affected text rather than passing raw content through.
    pub fn anonymize_batch_strings(&self, texts: &[&str]) -> Vec<String> {
        let mut ner_degraded = vec![false; texts.len()];
        match self.anonymize_batch(texts, &mut ner_degraded) {
            Ok(results) => results.into_iter().map(|r| r.text).collect(),
            Err(e) => {
                tracing::error!(error = %e, "anonymize_batch failed — blocking content");
                vec!["[PII_DETECTION_ERROR: content blocked]".to_string(); texts.len()]
            }
        }
    }

    /// Rehydrate a previously anonymized text, restoring original PII values.
    ///
    /// OPT-2: Uses round-robin starting slot selection.
    pub fn rehydrate_text(&self, text: &str) -> String {
        let n     = self.slots.len();
        let start = self.counter.fetch_add(1, Ordering::Relaxed) % n;
        for i in 0..n {
            let idx = (start + i) % n;
            if let Some(engine) = self.slots[idx].engine.try_lock() {
                return engine.rehydrate(text).unwrap_or_else(|_| text.to_string());
            }
        }
        let engine = self.slots[start].engine.lock();
        engine.rehydrate(text).unwrap_or_else(|_| text.to_string())
    }
}
