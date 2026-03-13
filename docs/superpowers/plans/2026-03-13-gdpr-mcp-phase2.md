# GDPR MCP Phase 2 — gline_rs L2 NER + HTTP Proxy + ClickHouse Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the existing Rust MCP server with Rust-native L2 NER (`gline_rs` + `gliner-pii-edge-v1.0` INT8), a concurrent `EnginePool`, an HTTP anonymization proxy (`:8080`) that strips PII from every OpenCode conversation before it reaches TensorZero, and a ClickHouse client with ring-buffer fault tolerance for the GDPR Art. 30 audit trail.

**Architecture:**
- `src/ner/mod.rs` — `GlinerNer`: loads INT8 ONNX once, shared via `Arc`, batches all texts in a single forward pass (~3ms total); degrades to absent if model missing (Invariant I2).
- `src/pii/pool.rs` — `EnginePool`: N slots (one per CPU), each with its own `Detector` (stateless clone), sharing the same `Vault` + `GlinerNer` via `Arc`. Round-robin `try_lock` eliminates single-mutex bottleneck for the proxy path.
- `src/proxy/mod.rs` — axum handler: collect all text slots from `messages`, `spawn_blocking → EnginePool::anonymize_batch`, write cleaned text back, forward to TensorZero, stream response verbatim (SSE-safe).
- `src/clients/clickhouse.rs` — HTTP writes to ClickHouse via `reqwest`. Circuit breaker (CB:10/10s) + 10 000-entry ring buffer: if CB is open, rows buffer; when CB closes they flush.
- `src/main.rs` — `tokio::select!` runs MCP stdio and HTTP proxy concurrently.

**Tech Stack:**
- Rust 2021 edition, tokio 1, axum 0.8
- `gline_rs 0.1` — ONNX GLiNER session, `TokenMode`
- `gliner-pii-edge-v1.0` INT8 ONNX (download via bash script, no Python)
- `unicode-segmentation 1` — word-boundary-safe chunking (400-word window, 50-word overlap)
- `num_cpus 1` — pool size + ONNX thread count
- `reqwest 0.12` — ClickHouse HTTP + proxy upstream forwarding
- `clickhouse/clickhouse-server:24.3` Docker

---

## Scope note

This plan covers changes to the **Rust `gdpr-mcp` crate only** (Phase 2).

**Out of scope (separate plans):**
- `gdpr-shield.ts` OpenCode plugin
- `opencode.json` / `.opencode/AGENTS.md` updates
- Docker Compose provisioning

---

## What already exists (do not rewrite)

| File | State | Notes |
|------|-------|-------|
| `src/pii/engine.rs` | EXISTS — L1 only | Add `gliner`, `try_clone`, `anonymize_batch` |
| `src/pii/mod.rs` | EXISTS | Add `pub mod pool; pub use pool::EnginePool;` |
| `src/audit/mod.rs` | EXISTS — SQLite | Unchanged |
| `src/resilience/mod.rs` | EXISTS — CB + BH + UpstreamGuard | Unchanged |
| `src/clients/metrics.rs` | EXISTS | Unchanged |
| `src/mcp/mod.rs` | EXISTS — 4 tools | Unchanged |
| `src/state.rs` | EXISTS | Add `engine_pool`, `clickhouse` fields |
| `src/main.rs` | EXISTS — stdio only | Add proxy + `tokio::select!` |
| `src/lib.rs` | EXISTS | Add `pub mod ner; pub mod proxy;` |

---

## File Map

```
gdpr-mcp/
├── Cargo.toml                          MODIFIED — + axum, gline_rs, unicode-seg, num_cpus, reqwest, ner feature
├── scripts/
│   └── download-gliner-edge.sh        NEW — curl INT8 ONNX from HuggingFace, no Python
├── src/
│   ├── ner/
│   │   └── mod.rs                     NEW — GlinerNer (load, detect_batch), chunk_text
│   ├── pii/
│   │   ├── engine.rs                  MODIFIED — gliner field, try_clone(), anonymize_batch()
│   │   ├── mod.rs                     MODIFIED — export EnginePool
│   │   └── pool.rs                    NEW — EnginePool: N-slot round-robin
│   ├── proxy/
│   │   └── mod.rs                     NEW — POST /openai/v1/chat/completions axum handler
│   ├── clients/
│   │   ├── mod.rs                     MODIFIED — pub mod clickhouse; pub use clickhouse::...
│   │   └── clickhouse.rs              NEW — GdprAuditRow, ClickHouseClient, ring buffer
│   ├── state.rs                       MODIFIED — engine_pool + clickhouse fields
│   ├── lib.rs                         MODIFIED — pub mod ner; pub mod proxy;
│   └── main.rs                        MODIFIED — load_production, proxy, tokio::select!
└── tests/
    ├── test_ner.rs                    NEW — GlinerNer degradation + chunk_text unit tests
    ├── test_pii_pool.rs               NEW — EnginePool concurrent slots, try_clone
    ├── test_clickhouse.rs             NEW — ring buffer overflow + flush logic
    └── test_proxy.rs                  NEW — batch collect/anonymize/write-back
```

---

## Chunk 1: Cargo.toml + Download Script + NER Layer

### Task 1: Cargo.toml additions + download script

**Files:**
- Modify: `gdpr-mcp/Cargo.toml`
- Create: `gdpr-mcp/scripts/download-gliner-edge.sh`

- [ ] **Step 1.1: Add new dependencies to Cargo.toml**

Open `gdpr-mcp/Cargo.toml` and add after the existing `[dependencies]` entries:

```toml
# HTTP proxy (conversation anonymization path)
axum        = "0.8"

# L2 NER — Rust-native GLiNER inference, no Python sidecar
gline_rs             = "0.1"
unicode-segmentation = "1"
num_cpus             = "1"

# ClickHouse audit trail + proxy upstream forwarding
reqwest     = { version = "0.12", features = ["json", "stream"] }

[features]
# Gate L2: build without --features ner avoids linking ONNX Runtime.
# The binary degrades gracefully to L1-only when model absent.
ner = []
```

- [ ] **Step 1.2: Verify Cargo.toml compiles**

```bash
cd gdpr-mcp && cargo build 2>&1 | head -20
```

Expected: compiles. `gline_rs 0.1` resolves from crates.io.

- [ ] **Step 1.3: Create the ONNX download script**

Create `gdpr-mcp/scripts/download-gliner-edge.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

OUTDIR="${GLINER_MODEL_DIR:-models/gliner-pii-edge}"
mkdir -p "$OUTDIR"

BASE="https://huggingface.co/knowledgator/gliner-pii-edge-v1.0/resolve/main"

echo "Downloading gliner-pii-edge-v1.0 INT8 ONNX to $OUTDIR …"
curl -fL "$BASE/onnx/model_quantized.onnx"  -o "$OUTDIR/model_int8.onnx"
curl -fL "$BASE/tokenizer.json"             -o "$OUTDIR/tokenizer.json"
curl -fL "$BASE/tokenizer_config.json"      -o "$OUTDIR/tokenizer_config.json"
curl -fL "$BASE/config.json"               -o "$OUTDIR/config.json"

echo "Done. Files in $OUTDIR:"
ls -lh "$OUTDIR"
```

```bash
chmod +x gdpr-mcp/scripts/download-gliner-edge.sh
```

- [ ] **Step 1.4: Commit**

```bash
git add gdpr-mcp/Cargo.toml gdpr-mcp/scripts/
git commit -m "feat(gdpr-mcp): add axum, gline_rs, reqwest deps + gliner download script"
```

---

### Task 2: `src/ner/mod.rs` — GlinerNer + chunk_text

**Files:**
- Create: `gdpr-mcp/src/ner/mod.rs`

- [ ] **Step 2.1: Write failing tests first**

Create `gdpr-mcp/tests/test_ner.rs`:

```rust
use gdpr_mcp::ner::{chunk_text, GlinerNer};

// ── chunk_text ────────────────────────────────────────────────────────────────

#[test]
fn test_short_text_is_not_chunked() {
    let chunks = chunk_text("Hello world foo bar", 400, 50);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].offset, 0);
}

#[test]
fn test_long_text_produces_overlapping_chunks() {
    // Build a 500-word text
    let text = (0..500).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" ");
    let chunks = chunk_text(&text, 400, 50);
    assert!(chunks.len() >= 2, "500 words should produce >= 2 chunks");
    // Second chunk offset must be > 0 (not a repeat of the first chunk)
    assert!(chunks[1].offset > 0);
    // Overlap: first chunk ends at token 400, second starts at token 350 (400-50)
    // so second chunk's offset < first chunk's byte length
    assert!(chunks[1].offset < chunks[0].text.len());
}

#[test]
fn test_chunks_cover_full_text() {
    let words = ["alpha", "beta", "gamma", "delta", "epsilon"];
    let text = words.join(" ");
    let chunks = chunk_text(&text, 3, 1);
    // Every word must appear in at least one chunk
    for word in &words {
        assert!(
            chunks.iter().any(|c| c.text.contains(word)),
            "{word} not covered by any chunk"
        );
    }
}

// ── GlinerNer graceful degradation ────────────────────────────────────────────

#[test]
fn test_gliner_load_returns_none_when_model_absent() {
    // "/nonexistent" will not have model_int8.onnx
    let result = GlinerNer::load("/nonexistent");
    assert!(result.is_ok(), "load() must not error on missing model (Invariant I2)");
    assert!(result.unwrap().is_none(), "must return None when model absent");
}
```

- [ ] **Step 2.2: Run tests — expect compile failure** (module doesn't exist yet)

```bash
cd gdpr-mcp && cargo test test_ner 2>&1 | head -20
```

Expected: `error[E0432]: unresolved import` — module absent.

- [ ] **Step 2.3: Create `src/ner/mod.rs`**

```rust
//! L2 NER via gline_rs + gliner-pii-edge-v1.0 INT8.
//!
//! - Single ONNX session shared across all EnginePool slots (`Arc<GlinerNer>`).
//! - Batched inference: all texts in one forward pass (~3ms total).
//! - 400-word chunking with 50-word overlap for texts that exceed positional embedding cap.
//! - Invariant I2: degrades to `None` if model absent — caller continues with L1 only.

use anyhow::Result;
use gline_rs::{GLiNER, Parameters, RuntimeParameters, TextInput, TokenMode};
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

const CHUNK_TOKENS:    usize = 400;   // positional embedding cap is 512 — stay below
const CHUNK_OVERLAP:   usize = 50;    // overlap catches entities straddling boundaries
const SCORE_THRESHOLD: f32   = 0.45;

/// Thread-safe GLiNER ONNX session. Load once at startup, share via `Arc`.
pub struct GlinerNer {
    model: GLiNER<TokenMode>,
}

// SAFETY: gline_rs ONNX sessions are Send + Sync per gline_rs docs.
unsafe impl Send for GlinerNer {}
unsafe impl Sync for GlinerNer {}

impl GlinerNer {
    /// Load INT8 model from `model_dir`.
    ///
    /// Returns `Ok(None)` if `model_int8.onnx` is absent — Invariant I2 (safe degradation).
    /// Returns `Err` only on file corruption or ONNX Runtime initialisation failure.
    pub fn load(model_dir: &str) -> Result<Option<Self>> {
        let onnx = format!("{model_dir}/model_int8.onnx");
        let tok  = format!("{model_dir}/tokenizer.json");

        if !std::path::Path::new(&onnx).exists() {
            tracing::warn!(path = %onnx, "GLiNER model absent — L2 NER disabled (Invariant I2)");
            return Ok(None);
        }

        let model = GLiNER::<TokenMode>::new(
            Parameters {
                labels:    PII_LABELS.iter().map(|s| s.to_string()).collect(),
                threshold: SCORE_THRESHOLD,
                ..Default::default()
            },
            RuntimeParameters {
                num_threads: num_cpus::get() as i16,
                ..Default::default()
            },
            &tok,
            &onnx,
        )?;
        tracing::info!(path = %onnx, "GLiNER model loaded (INT8, TokenMode)");
        Ok(Some(Self { model }))
    }

    /// Detect PII entities across a batch in a single ONNX forward pass.
    ///
    /// Long texts are split into overlapping chunks, results are merged back to original
    /// byte offsets, and duplicate spans (from overlap) are deduplicated.
    pub fn detect_batch(&self, texts: &[&str]) -> Result<Vec<Vec<NerEntity>>> {
        let chunked: Vec<Vec<Chunk>> = texts
            .iter()
            .map(|t| chunk_text(t, CHUNK_TOKENS, CHUNK_OVERLAP))
            .collect();

        let flat: Vec<&str> = chunked
            .iter()
            .flat_map(|cs| cs.iter().map(|c| c.text.as_str()))
            .collect();

        let output = self.model.inference(TextInput::from_str(&flat, PII_LABELS)?)?;

        let mut results: Vec<Vec<NerEntity>> = vec![vec![]; texts.len()];
        let mut flat_idx = 0usize;

        for (text_idx, chunks) in chunked.iter().enumerate() {
            // Track (start, end) pairs to suppress overlap duplicates
            let mut seen: std::collections::HashSet<(usize, usize)> = Default::default();
            for chunk in chunks {
                if flat_idx >= output.len() { break; }
                for e in &output[flat_idx] {
                    let abs_start = chunk.offset + e.start;
                    let abs_end   = chunk.offset + e.end;
                    if seen.insert((abs_start, abs_end)) {
                        results[text_idx].push(NerEntity {
                            text:  e.text.clone(),
                            label: e.label.clone(),
                            start: abs_start,
                            end:   abs_end,
                            score: e.score,
                        });
                    }
                }
                flat_idx += 1;
            }
        }
        Ok(results)
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
pub fn chunk_text(text: &str, max_tokens: usize, overlap: usize) -> Vec<Chunk> {
    let words: Vec<(usize, &str)> = text
        .split_word_bound_indices()
        .filter(|(_, w)| !w.trim().is_empty())
        .collect();

    if words.len() <= max_tokens {
        return vec![Chunk { text: text.to_string(), offset: 0 }];
    }

    let mut chunks = Vec::new();
    let mut start  = 0usize;

    while start < words.len() {
        let end   = (start + max_tokens).min(words.len());
        let s_b   = words[start].0;
        let e_b   = if end < words.len() { words[end].0 } else { text.len() };
        chunks.push(Chunk { text: text[s_b..e_b].to_string(), offset: s_b });
        if end == words.len() { break; }
        start = end.saturating_sub(overlap);
    }
    chunks
}
```

- [ ] **Step 2.4: Register module in `src/lib.rs`**

Add `pub mod ner;` to `src/lib.rs`:

```rust
pub mod ner;
```

- [ ] **Step 2.5: Run NER tests — expect PASS**

```bash
cd gdpr-mcp && cargo test test_ner 2>&1
```

Expected output:
```
test test_short_text_is_not_chunked ... ok
test test_long_text_produces_overlapping_chunks ... ok
test test_chunks_cover_full_text ... ok
test test_gliner_load_returns_none_when_model_absent ... ok

test result: ok. 4 passed; 0 failed
```

- [ ] **Step 2.6: Run full test suite — no regressions**

```bash
cd gdpr-mcp && cargo test 2>&1 | tail -10
```

Expected: all previously passing tests still pass.

- [ ] **Step 2.7: Commit**

```bash
git add gdpr-mcp/src/ner/ gdpr-mcp/src/lib.rs gdpr-mcp/tests/test_ner.rs
git commit -m "feat(gdpr-mcp): add gline_rs L2 NER layer with chunking and graceful degradation"
```

---

## Chunk 2: PiiEngine L2 Integration + EnginePool

### Task 3: Update `src/pii/engine.rs` — add L2 + batch + try_clone

**Files:**
- Modify: `gdpr-mcp/src/pii/engine.rs`

The existing `PiiEngine` has L1 only. We add:
- `gliner: Option<Arc<GlinerNer>>` field
- `has_ner: bool` field
- `load_production(vault_path, model_dir)` constructor
- `try_clone()` — cheap: new Detector, Arc::clone Vault + GlinerNer
- `anonymize_batch(texts, ner_degraded)` — L1 per-text + L2 single ONNX pass + merge
- Keep existing `anonymize()` as a single-item wrapper over `anonymize_batch`
- Keep existing `load_for_test()` unchanged (L1 only)

- [ ] **Step 3.1: Write failing tests**

Create `gdpr-mcp/tests/test_pii_pool.rs` (will grow in Task 4):

```rust
use gdpr_mcp::pii::PiiEngine;
use tempfile::tempdir;

fn make_test_engine() -> PiiEngine {
    let dir = tempdir().unwrap();
    let vault = dir.path().join("v.vault").to_str().unwrap().to_string();
    std::env::set_var("CLOAKPIPE_VAULT_KEY", "test-key-32-bytes-long-padding!!");
    PiiEngine::load_for_test(&vault).expect("engine must load")
}

#[test]
fn test_try_clone_produces_independent_engine() {
    let e1 = make_test_engine();
    let e2 = e1.try_clone().expect("try_clone must succeed");
    // Both engines must be independently usable
    let mut nd1 = false;
    let mut nd2 = false;
    let r1 = e1.anonymize("alice@example.com", &mut nd1).unwrap();
    let r2 = e2.anonymize("alice@example.com", &mut nd2).unwrap();
    // Pseudonyms must be the same because Vault is shared
    assert_eq!(r1.text, r2.text, "shared vault must produce identical pseudonyms");
}

#[test]
fn test_anonymize_batch_produces_one_result_per_input() {
    let mut engine = make_test_engine();
    let texts = &["alice@example.com", "no PII here", "bob@test.org"];
    let mut nd = false;
    let results = engine.anonymize_batch(texts, &mut nd).unwrap();
    assert_eq!(results.len(), 3);
    assert!(!results[0].text.contains("alice@example.com"));
    assert_eq!(results[1].text, "no PII here"); // passthrough
    assert!(!results[2].text.contains("bob@test.org"));
}

#[test]
fn test_anonymize_batch_ner_degraded_when_no_model() {
    let mut engine = make_test_engine(); // no model → L1 only
    let mut nd = false;
    engine.anonymize_batch(&["hello"], &mut nd).unwrap();
    assert!(nd, "ner_degraded must be true when no model loaded");
}
```

- [ ] **Step 3.2: Run tests — expect compile failure** (try_clone / anonymize_batch absent)

```bash
cd gdpr-mcp && cargo test test_pii_pool 2>&1 | head -20
```

Expected: `error[E0599]: no method named 'try_clone'` or similar.

- [ ] **Step 3.3: Update `src/pii/engine.rs`**

Replace the `PiiEngine` struct definition and impl. Keep the existing imports and `AnonymizeResult`. The key changes:

```rust
// Add to imports at top of file:
use std::sync::Arc;
use crate::ner::{GlinerNer, NerEntity};

// Updated struct:
pub struct PiiEngine {
    detector: Detector,                    // L1 — always runs (Invariant I1)
    vault:    Arc<Mutex<Vault>>,           // shared across all pool slots
    has_ner:  bool,
    gliner:   Option<Arc<GlinerNer>>,      // L2 — shared ONNX session (Invariant I2)
}
```

Add new methods to the existing `impl PiiEngine` block:

```rust
/// Production constructor: loads GLiNER from `model_dir` if present, degrades to L1 otherwise.
pub fn load_production(vault_path: &str, model_dir: &str) -> anyhow::Result<Self> {
    let detector = Detector::from_config(&Self::default_l1_config()?)?;
    let vault    = Vault::open(vault_path, Self::key_from_env())?;
    let gliner   = GlinerNer::load(model_dir)?;
    Ok(Self {
        detector,
        vault:   Arc::new(Mutex::new(vault)),
        has_ner: gliner.is_some(),
        gliner:  gliner.map(Arc::new),
    })
}

/// Cheap clone for EnginePool slot construction.
///
/// Each slot gets a fresh `Detector` (stateless, cheap to create).
/// `Vault` and `GlinerNer` are `Arc::clone` — pseudonym consistency and one ONNX session.
pub fn try_clone(&self) -> anyhow::Result<Self> {
    Ok(Self {
        detector: Detector::from_config(&Self::default_l1_config()?)?,
        vault:    Arc::clone(&self.vault),
        has_ner:  self.has_ner,
        gliner:   self.gliner.clone(),
    })
}

/// Batch anonymization — L1 per-text then L2 as a single ONNX forward pass.
///
/// Invariant I1: L1 failure returns `Err` (fatal — never pass raw text through).
/// Invariant I2: L2 failure sets `ner_degraded = true`, continues with L1 only.
pub fn anonymize_batch(
    &mut self,
    texts: &[&str],
    ner_degraded: &mut bool,
) -> anyhow::Result<Vec<AnonymizeResult>> {
    *ner_degraded = !self.has_ner;

    // L1 — always runs per-text
    let l1_entities: Vec<Vec<DetectedEntity>> = texts
        .iter()
        .map(|t| self.detector.detect(t))
        .collect::<anyhow::Result<_>>()?;

    // L2 — single batched ONNX pass (shared ONNX session, no per-request overhead)
    let l2_entities: Option<Vec<Vec<NerEntity>>> = match &self.gliner {
        Some(g) => match g.detect_batch(texts) {
            Ok(b)  => Some(b),
            Err(e) => {
                tracing::warn!(error = %e, "GLiNER batch failed — L1 only (Invariant I2)");
                *ner_degraded = true;
                None
            }
        },
        None => None,
    };

    let mut vault   = self.vault.lock().map_err(|_| anyhow::anyhow!("vault poisoned"))?;
    let mut results = Vec::with_capacity(texts.len());

    for (i, text) in texts.iter().enumerate() {
        let mut combined = l1_entities[i].clone();

        // Merge L2 NER entities into combined span list for deduplicated pseudonymization
        if let Some(ref l2) = l2_entities {
            for e in &l2[i] {
                combined.push(DetectedEntity {
                    text:     e.text.clone(),
                    label:    e.label.clone(),
                    start:    e.start,
                    end:      e.end,
                    score:    e.score,
                    source:   "gliner".into(),
                    ..Default::default()
                });
            }
        }

        if combined.is_empty() {
            results.push(AnonymizeResult {
                text:         text.to_string(),
                entities:     vec![],
                ner_degraded: *ner_degraded,
                pii_count:    0,
            });
            continue;
        }

        let pii_count     = combined.len();
        let pseudonymized = Replacer::pseudonymize(text, &combined, &mut vault)?;
        results.push(AnonymizeResult {
            text:         pseudonymized.text,
            entities:     pseudonymized.entities,
            ner_degraded: *ner_degraded,
            pii_count,
        });
    }
    Ok(results)
}
```

Ensure the existing `anonymize()` delegates to `anonymize_batch`:

```rust
pub fn anonymize(&mut self, text: &str, ner_degraded: &mut bool) -> anyhow::Result<AnonymizeResult> {
    let mut results = self.anonymize_batch(&[text], ner_degraded)?;
    Ok(results.remove(0))
}
```

Also update `load_for_test` to initialise the new fields:

```rust
pub fn load_for_test(vault_path: &str) -> anyhow::Result<Self> {
    let detector = Detector::from_config(&Self::default_l1_config()?)?;
    let vault    = Vault::open(vault_path, Self::key_from_env())?;
    Ok(Self {
        detector,
        vault: Arc::new(Mutex::new(vault)),
        has_ner: false,
        gliner:  None,
    })
}
```

- [ ] **Step 3.4: Run pii_pool tests — expect PASS**

```bash
cd gdpr-mcp && cargo test test_pii_pool 2>&1
```

Expected: 3 tests pass.

- [ ] **Step 3.5: Run full suite — no regressions**

```bash
cd gdpr-mcp && cargo test 2>&1 | tail -10
```

Expected: all existing tests still pass.

- [ ] **Step 3.6: Commit**

```bash
git add gdpr-mcp/src/pii/engine.rs gdpr-mcp/tests/test_pii_pool.rs
git commit -m "feat(gdpr-mcp): add gline_rs L2, anonymize_batch, try_clone to PiiEngine"
```

---

### Task 4: `src/pii/pool.rs` — EnginePool

**Files:**
- Create: `gdpr-mcp/src/pii/pool.rs`
- Modify: `gdpr-mcp/src/pii/mod.rs`

- [ ] **Step 4.1: Add pool tests to `tests/test_pii_pool.rs`**

Append to the existing test file:

```rust
use gdpr_mcp::pii::EnginePool;

#[test]
fn test_engine_pool_creates_n_slots() {
    let seed   = make_test_engine();
    let pool   = EnginePool::new(seed, 4).expect("pool must build");
    // anonymize_batch must work — verifies all slots are functional
    let result = pool.anonymize_batch(&["no PII here"]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], "no PII here");
}

#[test]
fn test_engine_pool_anonymizes_multiple_texts() {
    let seed = make_test_engine();
    let pool = EnginePool::new(seed, 2).expect("pool must build");
    let inputs = &["Contact alice@example.com", "hello world", "bob@test.org info"];
    let results = pool.anonymize_batch(inputs);
    assert_eq!(results.len(), 3);
    assert!(!results[0].contains("alice@example.com"), "alice must be pseudonymized");
    assert_eq!(results[1], "hello world", "clean text unchanged");
    assert!(!results[2].contains("bob@test.org"), "bob must be pseudonymized");
}

#[test]
fn test_engine_pool_shared_vault_produces_consistent_pseudonyms() {
    let seed = make_test_engine();
    let pool = EnginePool::new(seed, 4).expect("pool must build");
    // Same email in two separate calls must produce the same pseudonym
    let r1 = pool.anonymize_batch(&["alice@example.com"]);
    let r2 = pool.anonymize_batch(&["alice@example.com"]);
    assert_eq!(r1[0], r2[0], "shared vault must produce identical pseudonyms across calls");
}
```

- [ ] **Step 4.2: Run — expect compile failure** (EnginePool not yet created)

```bash
cd gdpr-mcp && cargo test test_engine_pool 2>&1 | head -10
```

- [ ] **Step 4.3: Create `src/pii/pool.rs`**

```rust
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
```

- [ ] **Step 4.4: Update `src/pii/mod.rs`**

Add the pool module and re-export `EnginePool`:

```rust
pub mod pool;
pub use pool::EnginePool;
```

- [ ] **Step 4.5: Run pool tests — expect PASS**

```bash
cd gdpr-mcp && cargo test test_engine_pool 2>&1
```

Expected: 3 tests pass.

- [ ] **Step 4.6: Run full suite**

```bash
cd gdpr-mcp && cargo test 2>&1 | tail -10
```

- [ ] **Step 4.7: Commit**

```bash
git add gdpr-mcp/src/pii/ gdpr-mcp/tests/test_pii_pool.rs
git commit -m "feat(gdpr-mcp): add EnginePool — N-slot round-robin concurrent anonymization"
```

---

## Chunk 3: ClickHouse Client

### Task 5: `src/clients/clickhouse.rs` — GdprAuditRow + ring buffer

**Files:**
- Create: `gdpr-mcp/src/clients/clickhouse.rs`
- Modify: `gdpr-mcp/src/clients/mod.rs`

The ClickHouse HTTP interface accepts `INSERT INTO table FORMAT JSONEachRow` with the row as a JSON body. No extra crate needed beyond `reqwest`.

Ring buffer design:
- `Mutex<VecDeque<GdprAuditRow>>` with capacity 10 000.
- When CB is open: push to ring (drop oldest if full).
- On successful write: flush buffered rows.

- [ ] **Step 5.1: Write failing tests**

Create `gdpr-mcp/tests/test_clickhouse.rs`:

```rust
use gdpr_mcp::clients::clickhouse::{ClickHouseClient, GdprAuditRow};

fn test_row() -> GdprAuditRow {
    GdprAuditRow {
        document_id:       "doc-123".into(),
        action:            "ingest".into(),
        pii_count_before:  5,
        pii_count_after:   0,
        ner_degraded:      false,
        processing_time_ms: 12,
        legal_basis:       "contract".into(),
        user_id:           "u1".into(),
        model_version:     "gliner-pii-edge-v1.0".into(),
    }
}

#[test]
fn test_row_serializes_to_json() {
    let row  = test_row();
    let json = serde_json::to_string(&row).expect("must serialize");
    assert!(json.contains("\"document_id\":\"doc-123\""));
    assert!(json.contains("\"action\":\"ingest\""));
    assert!(json.contains("\"pii_count_before\":5"));
}

#[tokio::test]
async fn test_buffer_fills_and_drops_oldest_on_overflow() {
    // Use a non-existent ClickHouse URL to force immediate CB failure
    let client = ClickHouseClient::new("http://127.0.0.1:19999");

    // Fire 10 001 records — first one must be dropped when capacity is exceeded
    for i in 0..10_001u32 {
        let mut row = test_row();
        row.pii_count_before = i;
        client.record(row).await;
    }

    // Ring buffer size must be capped at 10 000
    let buf_len = client.buffer_len();
    assert_eq!(buf_len, 10_000, "buffer must not exceed BUFFER_CAP");
}

#[tokio::test]
async fn test_record_does_not_panic_on_unreachable_server() {
    let client = ClickHouseClient::new("http://127.0.0.1:19999");
    // Must complete without panic — CB absorbs the failure
    client.record(test_row()).await;
    client.record(test_row()).await;
}
```

- [ ] **Step 5.2: Run — expect compile failure**

```bash
cd gdpr-mcp && cargo test test_clickhouse 2>&1 | head -10
```

- [ ] **Step 5.3: Create `src/clients/clickhouse.rs`**

```rust
//! ClickHouse audit client (GDPR Art. 30 immutable audit trail).
//!
//! Writes `GdprAuditRow` records via the ClickHouse HTTP interface
//! (`INSERT INTO gdpr_audit FORMAT JSONEachRow`).
//!
//! Fault tolerance:
//! - Circuit breaker: CB:10 failures / 10s cooldown.
//! - Ring buffer: up to 10 000 rows buffered when CB is open.
//! - Flush: buffer drains on the next successful write.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::resilience::CircuitBreaker;

const BUFFER_CAP: usize = 10_000;

// ── Row struct ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprAuditRow {
    pub document_id:        String,
    /// One of: "ingest", "query", "deanonymize", "delete"
    pub action:             String,
    pub pii_count_before:   u32,
    pub pii_count_after:    u32,
    pub ner_degraded:       bool,
    pub processing_time_ms: u32,
    /// One of: "consent", "contract", "legal_obligation", "vital_interest",
    ///         "public_task", "legitimate_interest"
    pub legal_basis:        String,
    pub user_id:            String,
    pub model_version:      String,
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct ClickHouseClient {
    url:    String,
    cb:     Arc<CircuitBreaker>,
    buffer: Mutex<VecDeque<GdprAuditRow>>,
    http:   reqwest::Client,
}

impl ClickHouseClient {
    /// Create a new client pointing at `url` (e.g. `"http://localhost:8123"`).
    pub fn new(url: &str) -> Arc<Self> {
        Arc::new(Self {
            url:    url.to_string(),
            cb:     CircuitBreaker::new("clickhouse", 10, Duration::from_secs(10)),
            buffer: Mutex::new(VecDeque::with_capacity(BUFFER_CAP)),
            http:   reqwest::Client::builder()
                        .timeout(Duration::from_secs(1))
                        .build()
                        .expect("reqwest client"),
        })
    }

    /// Record a GDPR audit event.
    ///
    /// - If CB is closed: write immediately; on success flush buffered rows.
    /// - If CB is open:   push to ring buffer (drops oldest entry if full).
    /// Never blocks the caller for more than the request timeout (1s).
    pub async fn record(&self, row: GdprAuditRow) {
        if self.cb.is_open() {
            self.push_to_buffer(row);
            return;
        }
        match self.write_one(&row).await {
            Ok(()) => {
                self.cb.record_success();
                self.flush_buffer().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "ClickHouse write failed — buffering row");
                self.cb.record_failure();
                self.push_to_buffer(row);
            }
        }
    }

    /// Number of rows currently in the ring buffer (test helper + metrics).
    pub fn buffer_len(&self) -> usize {
        self.buffer.lock().map(|b| b.len()).unwrap_or(0)
    }

    // ── private ───────────────────────────────────────────────────────────────

    fn push_to_buffer(&self, row: GdprAuditRow) {
        if let Ok(mut buf) = self.buffer.lock() {
            if buf.len() >= BUFFER_CAP {
                buf.pop_front(); // ring: drop oldest
                tracing::warn!("ClickHouse ring buffer full — dropped oldest entry");
            }
            buf.push_back(row);
        }
    }

    async fn write_one(&self, row: &GdprAuditRow) -> anyhow::Result<()> {
        let body = serde_json::to_string(row)?;
        let resp = self.http
            .post(format!("{}/", self.url))
            .query(&[("query", "INSERT INTO gdpr_audit FORMAT JSONEachRow")])
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!(
                "ClickHouse HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        Ok(())
    }

    async fn flush_buffer(&self) {
        // Drain under lock, then write without holding the lock
        let rows: Vec<GdprAuditRow> = {
            let mut buf = match self.buffer.lock() {
                Ok(b) => b,
                Err(_) => return,
            };
            buf.drain(..).collect()
        };

        for row in rows {
            if let Err(e) = self.write_one(&row).await {
                tracing::warn!(error = %e, "ClickHouse flush failed — re-buffering");
                self.cb.record_failure();
                self.push_to_buffer(row);
                break; // stop flushing on first failure
            }
            self.cb.record_success();
        }
    }
}
```

- [ ] **Step 5.4: Update `src/clients/mod.rs`**

```rust
pub mod clickhouse;
pub mod metrics;

pub use clickhouse::ClickHouseClient;
pub use metrics::{gather_text, metrics};
```

- [ ] **Step 5.5: Run ClickHouse tests — expect PASS**

```bash
cd gdpr-mcp && cargo test test_clickhouse 2>&1
```

Expected: 3 tests pass (write_one will fail with connection refused, CB absorbs it).

- [ ] **Step 5.6: Run full suite**

```bash
cd gdpr-mcp && cargo test 2>&1 | tail -10
```

- [ ] **Step 5.7: Commit**

```bash
git add gdpr-mcp/src/clients/ gdpr-mcp/tests/test_clickhouse.rs
git commit -m "feat(gdpr-mcp): ClickHouse client with ring buffer (CB:10/10s, 10k rows)"
```

---

## Chunk 4: HTTP Anonymization Proxy

### Task 6: `src/proxy/mod.rs`

**Files:**
- Create: `gdpr-mcp/src/proxy/mod.rs`

The proxy intercepts `POST /openai/v1/chat/completions`, batch-anonymizes all text content, and forwards to TensorZero while streaming the response back verbatim (SSE-safe).

- [ ] **Step 6.1: Write failing tests**

Create `gdpr-mcp/tests/test_proxy.rs`:

```rust
use gdpr_mcp::proxy::{collect_text_slots, write_text_slots, MessageContent, ChatMessage};
use serde_json::json;

fn text_msg(role: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role:    role.to_string(),
        content: MessageContent::Text(content.to_string()),
        extra:   json!({}),
    }
}

fn parts_msg(role: &str, texts: &[&str]) -> ChatMessage {
    use gdpr_mcp::proxy::ContentPart;
    let parts = texts.iter().map(|t| ContentPart {
        part_type: "text".to_string(),
        text:      Some(t.to_string()),
        extra:     json!({}),
    }).collect();
    ChatMessage { role: role.to_string(), content: MessageContent::Parts(parts), extra: json!({}) }
}

#[test]
fn test_collect_slots_from_text_messages() {
    let msgs = vec![
        text_msg("user",      "hello alice@example.com"),
        text_msg("assistant", "acknowledged"),
    ];
    let (texts, slots) = collect_text_slots(&msgs);
    assert_eq!(texts.len(), 2);
    assert_eq!(slots.len(), 2);
    assert_eq!(texts[0], "hello alice@example.com");
    assert_eq!(slots[0], (0usize, None));   // msg 0, no part index
    assert_eq!(slots[1], (1usize, None));
}

#[test]
fn test_collect_slots_from_parts_messages() {
    let msgs = vec![parts_msg("user", &["text A", "text B"])];
    let (texts, slots) = collect_text_slots(&msgs);
    assert_eq!(texts.len(), 2);
    assert_eq!(slots[0], (0usize, Some(0)));
    assert_eq!(slots[1], (0usize, Some(1)));
}

#[test]
fn test_write_slots_updates_text_messages() {
    let mut msgs = vec![text_msg("user", "original")];
    let slots = vec![(0usize, None)];
    write_text_slots(&mut msgs, &["cleaned"], &slots);
    match &msgs[0].content {
        MessageContent::Text(t) => assert_eq!(t, "cleaned"),
        _ => panic!("expected Text"),
    }
}

#[test]
fn test_write_slots_updates_parts_messages() {
    let mut msgs = vec![parts_msg("user", &["part0", "part1"])];
    let slots = vec![(0usize, Some(0)), (0usize, Some(1))];
    write_text_slots(&mut msgs, &["clean0", "clean1"], &slots);
    match &msgs[0].content {
        MessageContent::Parts(parts) => {
            assert_eq!(parts[0].text.as_deref(), Some("clean0"));
            assert_eq!(parts[1].text.as_deref(), Some("clean1"));
        }
        _ => panic!("expected Parts"),
    }
}

#[test]
fn test_collect_then_write_is_roundtrip() {
    let original = "hello@example.com is a PII";
    let mut msgs  = vec![text_msg("user", original)];
    let (texts, slots) = collect_text_slots(&msgs);
    let cleaned: Vec<String> = texts.iter().map(|t| t.replace("hello@example.com", "EMAIL_1")).collect();
    write_text_slots(&mut msgs, &cleaned, &slots);
    match &msgs[0].content {
        MessageContent::Text(t) => assert!(t.contains("EMAIL_1") && !t.contains("hello@example.com")),
        _ => panic!(),
    }
}
```

- [ ] **Step 6.2: Run — expect compile failure**

```bash
cd gdpr-mcp && cargo test test_proxy 2>&1 | head -10
```

- [ ] **Step 6.3: Create `src/proxy/mod.rs`**

```rust
//! HTTP anonymization proxy — `POST /openai/v1/chat/completions`.
//!
//! Pipeline:
//! 1. Collect all text slots from `messages` into a flat `Vec<String>`.
//! 2. `spawn_blocking` → `EnginePool::anonymize_batch` (L1 regex + L2 ONNX).
//! 3. Write cleaned strings back into `req.messages`, preserving structure.
//! 4. Forward to TensorZero, stream response verbatim (SSE-safe).
//!
//! Note: This proxy is transparent — OpenCode configures it as the `baseURL` for
//! the `hacienda` provider in `opencode.json`. All conversation traffic flows through it.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pii::EnginePool;

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
    pub text:      Option<String>,
    #[serde(flatten)]
    pub extra:     Value,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatMessage {
    pub role:    String,
    pub content: MessageContent,
    #[serde(flatten)]
    pub extra:   Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatCompletionRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(flatten)]
    pub extra: Value,
}

// ── Shared state for axum handler ─────────────────────────────────────────────

pub struct ProxyState {
    pub engine_pool:  Arc<EnginePool>,
    pub http_client:  reqwest::Client,
    pub upstream_url: String,   // e.g. "http://localhost:3000/openai/v1"
    pub upstream_key: String,
}

// ── Helper functions (pub for tests) ──────────────────────────────────────────

/// Collect all text content from messages into a flat Vec.
///
/// Returns `(texts, slots)` where each slot is `(msg_idx, part_idx?)`.
pub fn collect_text_slots(
    messages: &[ChatMessage],
) -> (Vec<String>, Vec<(usize, Option<usize>)>) {
    let mut texts = Vec::new();
    let mut slots = Vec::new();
    for (mi, msg) in messages.iter().enumerate() {
        match &msg.content {
            MessageContent::Text(t) => {
                texts.push(t.clone());
                slots.push((mi, None));
            }
            MessageContent::Parts(parts) => {
                for (pi, part) in parts.iter().enumerate() {
                    if part.part_type == "text" {
                        if let Some(t) = &part.text {
                            texts.push(t.clone());
                            slots.push((mi, Some(pi)));
                        }
                    }
                }
            }
        }
    }
    (texts, slots)
}

/// Write cleaned texts back into messages at the positions described by `slots`.
pub fn write_text_slots(
    messages:  &mut Vec<ChatMessage>,
    cleaned:   &[String],
    slots:     &[(usize, Option<usize>)],
) {
    for (clean, (mi, pi)) in cleaned.iter().zip(slots.iter()) {
        match pi {
            None => {
                messages[*mi].content = MessageContent::Text(clean.clone());
            }
            Some(p) => {
                if let MessageContent::Parts(ref mut parts) = messages[*mi].content {
                    if let Some(t) = &mut parts[*p].text {
                        *t = clean.clone();
                    }
                }
            }
        }
    }
}

// ── Axum handler ─────────────────────────────────────────────────────────────

pub async fn chat_completions(
    State(state): State<Arc<ProxyState>>,
    _headers:     HeaderMap,
    Json(mut req): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    // 1. Collect text slots
    let (raw_texts, slots) = collect_text_slots(&req.messages);

    // 2. spawn_blocking batch anonymize — CPU-bound (regex + ONNX)
    if !raw_texts.is_empty() {
        let pool   = Arc::clone(&state.engine_pool);
        let cloned = raw_texts.clone();

        let cleaned: Vec<String> = tokio::task::spawn_blocking(move || {
            pool.anonymize_batch(&cloned.iter().map(String::as_str).collect::<Vec<_>>())
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "spawn_blocking panic — blocking content");
            vec!["[PII_DETECTION_ERROR: content blocked]".to_string(); raw_texts.len()]
        });

        // 3. Write cleaned strings back
        write_text_slots(&mut req.messages, &cleaned, &slots);
    }

    // 4. Forward to TensorZero, stream response verbatim (SSE ✓)
    match state
        .http_client
        .post(format!("{}/chat/completions", state.upstream_url))
        .header("Authorization", format!("Bearer {}", state.upstream_key))
        .header("Content-Type", "application/json")
        .json(&req)
        .send()
        .await
    {
        Ok(resp) => {
            let status  = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);
            let mut headers = HeaderMap::new();
            for (k, v) in resp.headers() {
                if let (Ok(n), Ok(val)) = (
                    HeaderName::from_bytes(k.as_str().as_bytes()),
                    HeaderValue::from_bytes(v.as_bytes()),
                ) {
                    headers.insert(n, val);
                }
            }
            let mut r = Response::new(Body::from_stream(resp.bytes_stream()));
            *r.status_mut()  = status;
            *r.headers_mut() = headers;
            r
        }
        Err(e) => {
            let mut r = Response::new(Body::from(format!(r#"{{"error":"{e}"}}"#)));
            *r.status_mut() = StatusCode::BAD_GATEWAY;
            r
        }
    }
}
```

- [ ] **Step 6.4: Register module in `src/lib.rs`**

```rust
pub mod proxy;
```

- [ ] **Step 6.5: Run proxy tests — expect PASS**

```bash
cd gdpr-mcp && cargo test test_proxy 2>&1
```

Expected: 5 tests pass.

- [ ] **Step 6.6: Run full suite**

```bash
cd gdpr-mcp && cargo test 2>&1 | tail -10
```

- [ ] **Step 6.7: Commit**

```bash
git add gdpr-mcp/src/proxy/ gdpr-mcp/src/lib.rs gdpr-mcp/tests/test_proxy.rs
git commit -m "feat(gdpr-mcp): HTTP anonymization proxy — batch collect/anonymize/write-back/forward"
```

---

## Chunk 5: AppState + main.rs Wiring

### Task 7: Update `src/state.rs`

**Files:**
- Modify: `gdpr-mcp/src/state.rs`

- [ ] **Step 7.1: Update AppState to include `engine_pool` and `clickhouse`**

The new `AppState` adds two fields while keeping the existing ones intact:

```rust
use crate::{
    audit::DocAuditDb,
    clients::clickhouse::ClickHouseClient,
    pii::{EnginePool, PiiEngine},
    resilience::CircuitBreaker,
};

pub struct AppState {
    pub pii_engine:   Arc<Mutex<PiiEngine>>,
    pub db:           Arc<Mutex<Connection>>,
    pub doc_audit:    DocAuditDb,
    pub kreuzberg_cb: Arc<CircuitBreaker>,
    /// N-slot pool for the HTTP proxy path (concurrent anonymization).
    pub engine_pool:  Arc<EnginePool>,
    /// ClickHouse audit trail client (GDPR Art. 30). None if CLICKHOUSE_URL unset.
    pub clickhouse:   Option<Arc<ClickHouseClient>>,
}

impl AppState {
    pub fn new(pii_engine: PiiEngine, db: Connection) -> anyhow::Result<Self> {
        init_schema(&db)?;
        let db_arc = Arc::new(Mutex::new(db));

        // Pool size = num CPUs (regex + ONNX is CPU-bound; more slots than cores = contention)
        let pool_size   = num_cpus::get().max(2);
        let engine_pool = EnginePool::new(pii_engine.try_clone()?, pool_size)?;

        let clickhouse = std::env::var("CLICKHOUSE_URL").ok().map(|url| {
            tracing::info!(url = %url, "ClickHouse audit trail enabled");
            ClickHouseClient::new(&url)
        });

        Ok(Self {
            pii_engine: Arc::new(Mutex::new(pii_engine)),
            doc_audit:  DocAuditDb::from_shared(Arc::clone(&db_arc)),
            db:         db_arc,
            kreuzberg_cb: CircuitBreaker::new("kreuzberg", 5, Duration::from_secs(30)),
            engine_pool,
            clickhouse,
        })
    }
}
```

- [ ] **Step 7.2: Verify it compiles**

```bash
cd gdpr-mcp && cargo build 2>&1 | grep -E "^error" | head -10
```

Expected: no errors.

- [ ] **Step 7.3: Run full suite — no regressions**

```bash
cd gdpr-mcp && cargo test 2>&1 | tail -10
```

- [ ] **Step 7.4: Commit**

```bash
git add gdpr-mcp/src/state.rs
git commit -m "feat(gdpr-mcp): extend AppState with EnginePool and optional ClickHouseClient"
```

---

### Task 8: Update `src/main.rs`

**Files:**
- Modify: `gdpr-mcp/src/main.rs`

Changes:
- Use `PiiEngine::load_production(vault_path, model_dir)` instead of `load_for_test`.
- Start HTTP proxy via axum on `PROXY_ADDR` (default `0.0.0.0:8080`).
- `tokio::select!` to run MCP stdio + HTTP proxy concurrently.

- [ ] **Step 8.1: Update `src/main.rs`**

```rust
mod audit; mod clients; mod error; mod extraction;
mod mcp; mod ner; mod pii; mod proxy; mod resilience; mod state;

use std::sync::Arc;
use axum::{routing::post, Router};
use rmcp::{service::ServiceExt, transport::io::stdio};
use crate::{mcp::GdprServer, pii::PiiEngine, proxy::ProxyState, state::AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    if std::env::var("CLOAKPIPE_VAULT_KEY").map(|k| k.is_empty()).unwrap_or(true) {
        anyhow::bail!("CLOAKPIPE_VAULT_KEY must be set (32-byte AES key)");
    }

    let vault_path = std::env::var("GDPR_VAULT_PATH")
        .unwrap_or_else(|_| "gdpr_vault.db".into());
    let db_path    = std::env::var("GDPR_DB_PATH")
        .unwrap_or_else(|_| "gdpr_docs.db".into());
    let model_dir  = std::env::var("GLINER_MODEL_DIR")
        .unwrap_or_else(|_| "models/gliner-pii-edge".into());

    // load_production: L1 always, L2 if model present (Invariant I2)
    let pii_engine = PiiEngine::load_production(&vault_path, &model_dir)?;
    let db = rusqlite::Connection::open(&db_path)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    let state = Arc::new(AppState::new(pii_engine, db)?);

    // ── HTTP anonymization proxy (:8080) ──────────────────────────────────────
    let proxy_addr = std::env::var("PROXY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".into());
    let proxy_state = Arc::new(ProxyState {
        engine_pool:  Arc::clone(&state.engine_pool),
        http_client:  reqwest::Client::new(),
        upstream_url: std::env::var("TENSORZERO_URL")
            .unwrap_or_else(|_| "http://localhost:3000/openai/v1".into()),
        upstream_key: std::env::var("TENSORZERO_KEY")
            .unwrap_or_else(|_| "hacienda".into()),
    });
    let app      = Router::new()
        .route("/openai/v1/chat/completions", post(proxy::chat_completions))
        .with_state(proxy_state);
    let listener = tokio::net::TcpListener::bind(&proxy_addr).await?;
    tracing::info!(%proxy_addr, "anonymization proxy listening");

    // ── MCP stdio server ──────────────────────────────────────────────────────
    let server = GdprServer::new(Arc::clone(&state));
    tracing::info!("gdpr-mcp MCP server ready (stdio)");

    // Both run concurrently — if either exits, the process exits.
    tokio::select! {
        res = axum::serve(listener, app) => { res?; }
        res = async {
            let running = server.serve(stdio()).await?;
            running.waiting().await?;
            Ok::<_, anyhow::Error>(())
        } => { res?; }
    }
    Ok(())
}
```

- [ ] **Step 8.2: Build in release**

```bash
cd gdpr-mcp && cargo build --release 2>&1 | grep -E "^error" | head -10
```

Expected: no errors.

- [ ] **Step 8.3: Run full test suite**

```bash
cd gdpr-mcp && cargo test 2>&1
```

Expected: all tests pass. Count should be ≥ 47 (43 existing + 4 NER + 3 pool + 3 CH + 5 proxy = 58).

- [ ] **Step 8.4: Smoke test — binary starts and prints MCP ready**

```bash
CLOAKPIPE_VAULT_KEY="test-key-32-bytes-long-padding!!" \
RUST_LOG=info \
./target/release/gdpr-mcp &
sleep 1
kill %1
```

Expected log lines (on stderr):
```
INFO gdpr_mcp: anonymization proxy listening proxy_addr=0.0.0.0:8080
INFO gdpr_mcp: gdpr-mcp MCP server ready (stdio)
```

- [ ] **Step 8.5: Commit**

```bash
git add gdpr-mcp/src/main.rs
git commit -m "feat(gdpr-mcp): wire EnginePool + HTTP proxy + tokio::select in main.rs"
```

---

## Chunk 6: ClickHouse Schema + Integration

### Task 9: ClickHouse init SQL + wire record() into MCP handlers

**Files:**
- Create: `gdpr-mcp/config/clickhouse/init.sql`
- Modify: `gdpr-mcp/src/mcp/mod.rs` (add ClickHouse record calls)

- [ ] **Step 9.1: Create ClickHouse schema**

Create `gdpr-mcp/config/clickhouse/init.sql`:

```sql
CREATE TABLE IF NOT EXISTS gdpr_audit (
    id                   UUID DEFAULT generateUUIDv4(),
    timestamp            DateTime64(3) DEFAULT now64(),
    document_id          String,
    action               Enum8('ingest'=1,'query'=2,'deanonymize'=3,'delete'=4,'export'=5,'review'=6),
    entity_types         Array(String),
    entity_counts        Map(String, UInt32),
    user_id              String,
    legal_basis          Enum8(
        'consent'=1,'contract'=2,'legal_obligation'=3,
        'vital_interest'=4,'public_task'=5,'legitimate_interest'=6
    ),
    pii_count_before     UInt32,
    pii_count_after      UInt32,
    ner_degraded         UInt8,
    processing_time_ms   UInt32,
    model_version        String
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(timestamp)
ORDER BY (timestamp, document_id)
TTL timestamp + INTERVAL 7 YEAR;
```

Note: The `GdprAuditRow` Rust struct uses a subset of these columns. The full schema is for the DPO SQL queries.

- [ ] **Step 9.2: Wire `clickhouse.record()` into `gdpr_ingest` handler**

In `src/mcp/mod.rs`, after the audit record in `gdpr_ingest` (around line 198), add:

```rust
// ClickHouse audit trail (GDPR Art. 30) — fire-and-forget, non-blocking
if let Some(ch) = &self.state.clickhouse {
    let ch   = Arc::clone(ch);
    let d_id = doc_id.clone();
    let pc   = result.pii_count as u32;
    let nd   = ner_degraded;
    let ms   = anonymize_start.elapsed().as_millis() as u32;
    tokio::spawn(async move {
        ch.record(crate::clients::clickhouse::GdprAuditRow {
            document_id:        d_id,
            action:             "ingest".into(),
            pii_count_before:   pc,
            pii_count_after:    0,
            ner_degraded:       nd,
            processing_time_ms: ms,
            legal_basis:        "legitimate_interest".into(),
            user_id:            String::new(),
            model_version:      "gliner-pii-edge-v1.0".into(),
        }).await;
    });
}
```

- [ ] **Step 9.3: Wire `clickhouse.record()` into `gdpr_delete` handler**

In the `Ok(_) =>` branch of `gdpr_delete`, after `metrics().deletes.inc()`:

```rust
if let Some(ch) = &self.state.clickhouse {
    let ch   = Arc::clone(ch);
    let d_id = doc_id.clone();
    tokio::spawn(async move {
        ch.record(crate::clients::clickhouse::GdprAuditRow {
            document_id:        d_id,
            action:             "delete".into(),
            pii_count_before:   0,
            pii_count_after:    0,
            ner_degraded:       false,
            processing_time_ms: 0,
            legal_basis:        "legal_obligation".into(),
            user_id:            String::new(),
            model_version:      String::new(),
        }).await;
    });
}
```

- [ ] **Step 9.4: Build and test**

```bash
cd gdpr-mcp && cargo build 2>&1 | grep "^error" | head -10
cd gdpr-mcp && cargo test 2>&1 | tail -10
```

Expected: no errors, all tests pass.

- [ ] **Step 9.5: Commit**

```bash
git add gdpr-mcp/config/ gdpr-mcp/src/mcp/mod.rs
git commit -m "feat(gdpr-mcp): wire ClickHouse audit records into gdpr_ingest and gdpr_delete"
```

---

### Task 10: Final verification + PR

- [ ] **Step 10.1: Full test suite with count**

```bash
cd gdpr-mcp && cargo test 2>&1
```

Expected: ≥ 55 tests, 0 failed.

- [ ] **Step 10.2: Release build — no warnings**

```bash
cd gdpr-mcp && cargo build --release 2>&1 | grep -cE "^warning" || true
```

Expected: 0 warnings (or only suppressed ones from `[lints.rust]`).

- [ ] **Step 10.3: Check binary size is reasonable**

```bash
ls -lh gdpr-mcp/target/release/gdpr-mcp
```

Expected: < 50 MB (without ONNX Runtime linkage). With `--features ner` it will be larger due to ONNX RT.

- [ ] **Step 10.4: Final commit + push**

```bash
git add -u
git commit -m "feat(gdpr-mcp): phase 2 complete — gline_rs L2, EnginePool, HTTP proxy, ClickHouse"
git push origin feat/gdpr-mcp
```

---

## Environment Variables Reference

| Variable | Default | Purpose |
|----------|---------|---------|
| `CLOAKPIPE_VAULT_KEY` | **required** | 32-byte AES-256-GCM key for the pseudonym vault |
| `GDPR_VAULT_PATH` | `gdpr_vault.db` | Path to vault SQLite file |
| `GDPR_DB_PATH` | `gdpr_docs.db` | Path to document/audit SQLite |
| `GLINER_MODEL_DIR` | `models/gliner-pii-edge` | Path to INT8 ONNX model dir (optional — degrades to L1 if absent) |
| `CLICKHOUSE_URL` | _(unset = disabled)_ | ClickHouse HTTP URL, e.g. `http://localhost:8123` |
| `TENSORZERO_URL` | `http://localhost:3000/openai/v1` | Upstream for proxy forwarding |
| `TENSORZERO_KEY` | `hacienda` | Bearer token for upstream |
| `PROXY_ADDR` | `0.0.0.0:8080` | Bind address for HTTP proxy |
| `RUST_LOG` | _(unset)_ | Tracing filter, e.g. `info,gdpr_mcp=debug` |

---

## What's Next (separate plans)

| Plan | Subsystem |
|------|-----------|
| `2026-03-13-gdpr-opencode-plugin.md` | `gdpr-shield.ts` TypeScript plugin + `opencode.json` + `AGENTS.md` |
| `2026-03-13-gdpr-docker-compose.md` | Docker Compose with ClickHouse, kreuzberg, TensorZero, cloakpipe-mcp |
