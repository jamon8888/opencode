//! Request ID injection via tower-http.

use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

pub fn make() -> (
    SetRequestIdLayer<MakeRequestUuid>,
    PropagateRequestIdLayer,
) {
    let header = axum::http::HeaderName::from_static("x-request-id");
    (
        SetRequestIdLayer::new(header.clone(), MakeRequestUuid),
        PropagateRequestIdLayer::new(header),
    )
}
