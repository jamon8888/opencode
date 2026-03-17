//! Metering middleware: cap enforcement (pre-request) + UsageEvent emission (post-request).

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use tower::{Layer, Service};

use gdpr_billing::{BillingSnapshot, EventType, MeteringRecord, UsageCap, UsageEvent};
use crate::state::{AppState, AuthContext};

// ── Layer ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MeterLayer {
    state: AppState,
}

impl MeterLayer {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

impl<S> Layer<S> for MeterLayer {
    type Service = MeterService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        MeterService { inner, state: self.state.clone() }
    }
}

// ── Service ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MeterService<S> {
    inner: S,
    state: AppState,
}

impl<S, B> Service<Request<B>> for MeterService<S>
where
    S: Service<Request<B>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response;
    type Error    = S::Error;
    type Future   = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        let state = self.state.clone();

        Box::pin(async move {
            // Extract AuthContext (set by AuthLayer); skip metering if anonymous
            let auth = match req.extensions().get::<AuthContext>().cloned() {
                Some(a) => a,
                None => return inner.call(req).await,
            };

            // ── Pre-request: cap check ────────────────────────────────────────
            let snapshot = state
                .snapshot_cache
                .get(&auth.tenant_id)
                .map(|e| e.0.clone())
                .unwrap_or_else(|| BillingSnapshot {
                    tenant_id: auth.tenant_id.clone(),
                    period_ym: BillingSnapshot::current_period_ym(),
                    ..Default::default()
                });

            if let Err(e) = (UsageCap { plan: auth.plan.clone() }).check(&snapshot) {
                let body = serde_json::json!({
                    "type":   "https://api.gdpr.dev/errors/usage-cap-exceeded",
                    "title":  "Usage Cap Exceeded",
                    "status": 429,
                    "detail": e.to_string(),
                });
                let mut resp = axum::Json(body).into_response();
                *resp.status_mut() = StatusCode::TOO_MANY_REQUESTS;
                resp.headers_mut().insert(
                    "Retry-After",
                    "60".parse().unwrap(),
                );
                return Ok(resp);
            }

            // ── Call handler ──────────────────────────────────────────────────
            let start = Instant::now();
            let resp  = inner.call(req).await?;
            let latency_ms = start.elapsed().as_millis() as u32;

            // ── Post-request: emit UsageEvent ─────────────────────────────────
            let metering = resp.extensions().get::<MeteringRecord>().cloned();
            let request_id = resp
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            if let Some(rec) = metering {
                if let Some(event_type) = rec.event_type {
                    let event = UsageEvent {
                        tenant_id:     auth.tenant_id.clone(),
                        api_key_id:    auth.api_key_id.clone(),
                        request_id,
                        event_type,
                        document_id:   rec.document_id,
                        chars_in:      rec.chars_in,
                        chars_out:     rec.chars_out,
                        doc_count:     rec.doc_count,
                        chunk_count:   0,
                        ai_tokens_in:  rec.ai_tokens_in,
                        ai_tokens_out: rec.ai_tokens_out,
                        ner_tier:      rec.ner_tier,
                        latency_ms,
                    };
                    // NOTE: Meter::record is synchronous (fire-and-forget via tokio::spawn internally)
                    state.meter.record(event);

                    // Optimistic cache update
                    state.snapshot_cache
                        .entry(auth.tenant_id.clone())
                        .and_modify(|(snap, ts)| {
                            snap.total_docs          += rec.doc_count as u64;
                            snap.total_chars_in      += rec.chars_in;
                            snap.total_rag_queries   += if event_type == EventType::Search { 1 } else { 0 };
                            snap.total_ai_tokens_in  += rec.ai_tokens_in as u64;
                            snap.total_ai_tokens_out += rec.ai_tokens_out as u64;
                            *ts = Instant::now();
                        })
                        .or_insert_with(|| (snapshot, Instant::now()));
                }
            }

            Ok(resp)
        })
    }
}
