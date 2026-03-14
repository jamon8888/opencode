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
