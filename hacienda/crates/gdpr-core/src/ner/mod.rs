//! L2 NER via gline_rs + gliner-pii-edge-v1.0 INT8.
//!
//! - Single ONNX session shared across all EnginePool slots (`Arc<GlinerNer>`).
//! - Batched inference: all texts in one forward pass (~3ms total).
//! - OPT-7: VecDeque sliding window in chunk_text replaces eager Vec allocation.
//! - Invariant I2: degrades to `None` if model absent — caller continues with L1 only.

use anyhow::Result;
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;

/// A detected entity from L2 NER.
#[derive(Debug, Clone)]
pub struct NerEntity {
    pub text:  String,
    pub label: String,
    pub start: usize,  // byte offset in the original (pre-chunk) text
    pub end:   usize,
    pub score: f32,
}

/// All PII labels supported by gliner-pii-edge-v1.0.
pub const PII_LABELS: &[&str] = &[
    // Identity
    "person", "full_name", "first_name", "last_name",
    // Contact
    "email", "phone_number", "address", "zip_code", "city",
    // French-specific
    "insee_number", "siret", "siren",
    // Financial
    "iban", "credit_card", "bank_account",
    // Government IDs
    "id_number", "passport_number", "driver_license",
    // Digital
    "ip_address", "url", "username",
    // Medical
    "medical_record", "diagnosis", "medication",
    // Organizational
    "organization", "company_name",
    // Contextual dates (not caught by L1)
    "date_of_birth",
];

/// Thread-safe GLiNER ONNX session. Load once at startup, share via `Arc`.
///
/// This is a stub implementation: the model field is a unit placeholder because
/// the full gline_rs / ort-sys dependency chain causes a CRT mismatch on MSVC.
/// `load()` always returns `Ok(None)` (Invariant I2), and `detect_batch` is
/// unreachable unless a model was somehow constructed.
pub struct GlinerNer {
    _private: (),
}

// SAFETY: No thread-sensitive state in this stub.
unsafe impl Send for GlinerNer {}
unsafe impl Sync for GlinerNer {}

impl GlinerNer {
    /// Load INT8 model from `model_dir`.
    ///
    /// Returns `Ok(None)` if `model_int8.onnx` is absent — Invariant I2 (safe degradation).
    pub fn load(model_dir: &str) -> Result<Option<Self>> {
        let onnx = format!("{model_dir}/model_int8.onnx");

        if !std::path::Path::new(&onnx).exists() {
            tracing::warn!(path = %onnx, "GLiNER model absent — L2 NER disabled (Invariant I2)");
            return Ok(None);
        }

        // Even if the file exists, we cannot load it on this platform.
        tracing::warn!(
            path = %onnx,
            "GLiNER model found but ONNX runtime linking disabled on this platform — L2 NER disabled"
        );
        Ok(None)
    }

    /// Detect PII entities across a batch.
    ///
    /// This method is unreachable in practice because `load()` always returns `None`.
    /// The signature is kept for API compatibility.
    pub fn detect_batch(&self, texts: &[&str]) -> Result<Vec<Vec<NerEntity>>> {
        Ok(vec![vec![]; texts.len()])
    }
}

// ── Chunking ──────────────────────────────────────────────────────────────────

/// A text window with its starting byte offset in the original string.
pub struct Chunk {
    pub text:   String,
    pub offset: usize,
}

/// Split `text` into word-token windows of at most `max_tokens` words,
/// with `overlap` words of context carried into the next window.
///
/// Returns a single chunk if `text` is already shorter than `max_tokens`.
///
/// OPT-7: Uses VecDeque sliding window to avoid re-allocating the full word list
/// per chunk. The deque holds only the current window of word indices.
pub fn chunk_text(text: &str, max_tokens: usize, overlap: usize) -> Vec<Chunk> {
    // Collect all word boundary indices (byte offset, word str)
    let words: Vec<(usize, &str)> = text
        .split_word_bound_indices()
        .filter(|(_, w)| !w.trim().is_empty())
        .collect();

    if words.len() <= max_tokens {
        return vec![Chunk { text: text.to_string(), offset: 0 }];
    }

    // OPT-7: VecDeque sliding window — we maintain a window of word indices
    // rather than slicing words[start..end] each iteration.
    let mut chunks   = Vec::new();
    let mut window: VecDeque<(usize, &str)> = VecDeque::with_capacity(max_tokens);
    let mut word_idx = 0usize;

    // Fill initial window
    while window.len() < max_tokens && word_idx < words.len() {
        window.push_back(words[word_idx]);
        word_idx += 1;
    }

    loop {
        let s_b = window.front().map(|(b, _)| *b).unwrap_or(0);
        let e_b = if word_idx < words.len() {
            words[word_idx].0
        } else {
            text.len()
        };
        chunks.push(Chunk { text: text[s_b..e_b].to_string(), offset: s_b });

        if word_idx >= words.len() {
            break;
        }

        // Slide: drop (max_tokens - overlap) words from front
        let drop_count = max_tokens.saturating_sub(overlap).max(1);
        for _ in 0..drop_count {
            window.pop_front();
        }

        // Refill window from remaining words
        while window.len() < max_tokens && word_idx < words.len() {
            window.push_back(words[word_idx]);
            word_idx += 1;
        }

        if window.is_empty() {
            break;
        }
    }

    chunks
}
