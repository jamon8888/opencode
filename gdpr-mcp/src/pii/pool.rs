//! EnginePool: eliminates `Arc<Mutex<PiiEngine>>` single-lock bottleneck on the proxy path.
//!
//! Each slot has its own `Detector` (stateless, cheap clone).
//! `Vault` and `GlinerNer` are `Arc`-shared — pseudonym consistency and one ONNX session.
//!
//! **MUST** call `anonymize_batch` via `spawn_blocking` — L1 regex + L2 ONNX is CPU-bound.

use std::sync::Arc;
use crate::pii::PiiEngine;

struct Slot {
    engine: std::sync::Mutex<PiiEngine>,
}

pub struct EnginePool {
    slots: Vec<Arc<Slot>>,
}

impl EnginePool {
    /// Build `size` independent slots from a seed via `try_clone`.
    /// All slots share the same `Vault` (pseudonym consistency) and `GlinerNer` (one ONNX session).
    pub fn new(seed: PiiEngine, size: usize) -> anyhow::Result<Arc<Self>> {
        let size  = size.max(1);
        let slots = (0..size)
            .map(|_| {
                Ok(Arc::new(Slot {
                    engine: std::sync::Mutex::new(seed.try_clone()?),
                }))
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(Arc::new(Self { slots }))
    }

    /// Anonymize a batch of texts, returning one cleaned string per input.
    ///
    /// Tries each slot in order with `try_lock` — takes the first available without blocking.
    /// Falls back to a blocking lock on slot 0 if all are busy.
    ///
    /// On L1 detection failure (Invariant I1): returns `"[PII_DETECTION_ERROR: content blocked]"`
    /// for every affected text rather than passing raw content through.
    pub fn anonymize_batch(&self, texts: &[&str]) -> Vec<String> {
        for slot in &self.slots {
            if let Ok(mut engine) = slot.engine.try_lock() {
                return Self::run(&mut engine, texts);
            }
        }
        // All slots busy — block on slot 0 rather than reject
        let mut engine = self.slots[0].engine.lock().expect("pool slot 0 poisoned");
        Self::run(&mut engine, texts)
    }

    /// Rehydrate a previously anonymized text, restoring original PII values.
    ///
    /// Uses any available pool slot (rehydration is read-only on the vault — any slot works).
    /// Falls back to slot 0 if all slots are busy.
    pub fn rehydrate_text(&self, text: &str) -> String {
        for slot in &self.slots {
            if let Ok(engine) = slot.engine.try_lock() {
                return engine.rehydrate(text).unwrap_or_else(|_| text.to_string());
            }
        }
        let engine = self.slots[0].engine.lock().expect("pool slot 0 poisoned");
        engine.rehydrate(text).unwrap_or_else(|_| text.to_string())
    }

    fn run(engine: &mut PiiEngine, texts: &[&str]) -> Vec<String> {
        let mut ner_degraded = false;
        match engine.anonymize_batch(texts, &mut ner_degraded) {
            Ok(results) => results.into_iter().map(|r| r.text).collect(),
            Err(e) => {
                // Invariant I1: never pass raw text through on detection failure
                tracing::error!(error = %e, "anonymize_batch failed — blocking content");
                vec!["[PII_DETECTION_ERROR: content blocked]".to_string(); texts.len()]
            }
        }
    }
}
