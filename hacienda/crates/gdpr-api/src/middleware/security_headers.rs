//! Security headers middleware — adds hardened headers to every response.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::http::{HeaderValue, Request};
use axum::response::Response;
use tower::{Layer, Service};

// ── Layer ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub struct SecurityHeadersLayer;

impl<S> Layer<S> for SecurityHeadersLayer {
    type Service = SecurityHeadersService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        SecurityHeadersService { inner }
    }
}

// ── Service ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SecurityHeadersService<S> {
    inner: S,
}

impl<S, B> Service<Request<B>> for SecurityHeadersService<S>
where
    S: Service<Request<B>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            let mut resp = inner.call(req).await?;
            let headers = resp.headers_mut();

            headers.insert(
                "strict-transport-security",
                HeaderValue::from_static("max-age=31536000; includeSubDomains"),
            );
            headers.insert(
                "x-content-type-options",
                HeaderValue::from_static("nosniff"),
            );
            headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
            headers.insert(
                "content-security-policy",
                HeaderValue::from_static("default-src 'none'"),
            );
            headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));

            Ok(resp)
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::Router;
    use axum::routing::get;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_security_headers_present() {
        let app = Router::new()
            .route("/test", get(|| async { "ok" }))
            .layer(SecurityHeadersLayer);

        let req = Request::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(
            resp.headers().get("strict-transport-security").unwrap(),
            "max-age=31536000; includeSubDomains"
        );
        assert_eq!(
            resp.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
        assert_eq!(
            resp.headers().get("content-security-policy").unwrap(),
            "default-src 'none'"
        );
        assert_eq!(
            resp.headers().get("referrer-policy").unwrap(),
            "no-referrer"
        );
    }
}
