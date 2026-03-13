use kreuzberg::{ExtractionConfig, PDF_MIME_TYPE, DOCX_MIME_TYPE};

/// Output of a document extraction.
pub struct ExtractionOutput {
    pub content: String,
    pub quality_score: f64,
}

/// Extract text from a file path, with language hint.
pub async fn extract_document(
    path: &std::path::Path,
    _language: &str,
) -> anyhow::Result<ExtractionOutput> {
    let path_str = path.to_str().ok_or_else(|| anyhow::anyhow!("invalid path"))?;
    let content = extract_text(path_str).await?;
    Ok(ExtractionOutput { content, quality_score: 1.0 })
}

/// Extract from raw bytes with an explicit MIME type.
pub async fn extract_document_bytes(
    data: &[u8],
    mime_type: &str,
    _language: &str,
) -> anyhow::Result<ExtractionOutput> {
    let config = ExtractionConfig::default();
    let result = kreuzberg::extract_bytes(data, mime_type, &config).await?;
    Ok(ExtractionOutput { content: result.content, quality_score: 1.0 })
}

/// Extract plain text from a file at `path`.
pub async fn extract_text(path: &str) -> anyhow::Result<String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "pdf" => extract_via_kreuzberg(path, PDF_MIME_TYPE).await,
        "docx" => extract_via_kreuzberg(path, DOCX_MIME_TYPE).await,
        "odt" => extract_via_kreuzberg(path, "application/vnd.oasis.opendocument.text").await,
        "png" => extract_via_kreuzberg(path, "image/png").await,
        "jpg" | "jpeg" => extract_via_kreuzberg(path, "image/jpeg").await,
        "tiff" | "tif" => extract_via_kreuzberg(path, "image/tiff").await,
        _ => Ok(tokio::fs::read_to_string(path).await?),
    }
}

async fn extract_via_kreuzberg(path: &str, mime_type: &str) -> anyhow::Result<String> {
    let bytes = tokio::fs::read(path).await?;
    let config = ExtractionConfig::default();
    let result = kreuzberg::extract_bytes(&bytes, mime_type, &config).await?;
    Ok(result.content)
}
