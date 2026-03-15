# ── Stage 1: builder ────────────────────────────────────────────────────────
FROM rust:1.82-slim AS builder

WORKDIR /workspace

RUN apt-get update \
 && apt-get install -y --no-install-recommends git pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates/gdpr-core/Cargo.toml  crates/gdpr-core/Cargo.toml
COPY crates/gdpr-api/Cargo.toml   crates/gdpr-api/Cargo.toml
COPY crates/gdpr-mcp/Cargo.toml   crates/gdpr-mcp/Cargo.toml
COPY crates/gdpr-billing/Cargo.toml crates/gdpr-billing/Cargo.toml

RUN mkdir -p crates/gdpr-core/src crates/gdpr-api/src crates/gdpr-mcp/src crates/gdpr-billing/src \
 && echo 'fn main(){}' > crates/gdpr-api/src/main.rs \
 && echo 'fn main(){}' > crates/gdpr-mcp/src/main.rs \
 && echo '' > crates/gdpr-core/src/lib.rs \
 && echo '' > crates/gdpr-billing/src/lib.rs

RUN cargo build --release --bin gdpr-mcp 2>/dev/null || true

COPY crates/ crates/
RUN touch crates/gdpr-mcp/src/main.rs \
 && cargo build --release --bin gdpr-mcp

# ── Stage 2: runtime ────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates netcat-openbsd \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /workspace/target/release/gdpr-mcp ./gdpr-mcp

RUN mkdir -p /data /models

ENV PROXY_ADDR=0.0.0.0:8080 \
    GDPR_DB_PATH=/data/gdpr_docs.db \
    GDPR_VAULT_PATH=/data/gdpr_vault.db \
    GLINER_MODEL_DIR=/models/gliner-pii-edge \
    RUST_LOG=info

EXPOSE 8080

# TCP health check on the proxy port (gdpr-mcp has no HTTP /health endpoint)
HEALTHCHECK --interval=10s --timeout=5s --start-period=30s --retries=3 \
    CMD sh -c 'printf "GET / HTTP/1.0\r\n\r\n" | nc -w2 localhost 8080 || exit 1'

CMD ["./gdpr-mcp"]
