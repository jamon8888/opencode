# ── Stage 1: builder ────────────────────────────────────────────────────────
FROM rust:1.82-slim AS builder

WORKDIR /workspace

# git: required for cloakpipe-core git dep
# pkg-config + libssl-dev: required by some transitive deps at compile time
RUN apt-get update \
 && apt-get install -y --no-install-recommends git pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*

# Copy workspace manifest files first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/gdpr-core/Cargo.toml  crates/gdpr-core/Cargo.toml
COPY crates/gdpr-api/Cargo.toml   crates/gdpr-api/Cargo.toml
COPY crates/gdpr-mcp/Cargo.toml   crates/gdpr-mcp/Cargo.toml
COPY crates/gdpr-billing/Cargo.toml crates/gdpr-billing/Cargo.toml

# Create stub lib/main files so cargo can resolve the dep graph without full source
RUN mkdir -p crates/gdpr-core/src crates/gdpr-api/src crates/gdpr-mcp/src crates/gdpr-billing/src \
 && echo 'fn main(){}' > crates/gdpr-api/src/main.rs \
 && echo 'fn main(){}' > crates/gdpr-mcp/src/main.rs \
 && echo '' > crates/gdpr-core/src/lib.rs \
 && echo '' > crates/gdpr-billing/src/lib.rs

# Pre-fetch + compile deps (cached layer unless Cargo.toml/Cargo.lock changes)
RUN cargo build --release --bin gdpr-api 2>/dev/null || true

# Now copy full source and build for real
COPY crates/ crates/
RUN touch crates/gdpr-api/src/main.rs \
 && cargo build --release --bin gdpr-api

# ── Stage 2: runtime ────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates wget \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /workspace/target/release/gdpr-api ./gdpr-api

# Runtime directories (mounted via volumes in compose)
RUN mkdir -p /data /models

ENV BIND_ADDR=0.0.0.0:8443 \
    GDPR_DB_PATH=/data/gdpr-api.db \
    GDPR_VAULT_PATH=/data/gdpr_vault.db \
    GLINER_MODEL_DIR=/models/gliner-pii-edge \
    RUST_LOG=info

EXPOSE 8443

HEALTHCHECK --interval=10s --timeout=5s --start-period=30s --retries=3 \
    CMD wget -qO- http://localhost:8443/health | grep -q '"status":"ok"' || exit 1

CMD ["./gdpr-api"]
