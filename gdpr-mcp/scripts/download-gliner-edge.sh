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
