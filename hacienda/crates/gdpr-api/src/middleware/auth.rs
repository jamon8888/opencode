//! Bearer token auth middleware with DashMap cache + JWT bootstrap support.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::Request;
use axum::response::{IntoResponse, Response};
use tower::{Layer, Service};

use crate::error::ApiError;
use crate::state::{AppState, AuthContext, CachedKey};
use gdpr_billing::Plan;

// ── Layer ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AuthLayer {
    state: AppState,
}

impl AuthLayer {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        AuthService {
            inner,
            state: self.state.clone(),
        }
    }
}

// ── Service ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AuthService<S> {
    inner: S,
    state: AppState,
}

impl<S, B> Service<Request<B>> for AuthService<S>
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

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);
        let state = self.state.clone();

        Box::pin(async move {
            // Extract Bearer token
            let token = match extract_bearer(req.headers()) {
                Some(t) => t,
                None => return Ok(ApiError::Unauthorized.into_response()),
            };

            // Try JWT first (fast path for bootstrap tokens)
            if let Some(ctx) = try_jwt(&token, &state.jwt_secret) {
                req.extensions_mut().insert(ctx);
                return inner.call(req).await;
            }

            // API key flow
            match authenticate_api_key(&token, &state).await {
                Ok(ctx) => {
                    req.extensions_mut().insert(ctx);
                    inner.call(req).await
                }
                Err(err) => Ok(err.into_response()),
            }
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn extract_bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|s| s.to_string())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ── JWT bootstrap ────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct Claims {
    pub sub: String,
    pub scope: String,
    pub exp: usize,
}

fn try_jwt(token: &str, secret: &str) -> Option<AuthContext> {
    if secret.is_empty() {
        return None;
    }
    let key = jsonwebtoken::DecodingKey::from_secret(secret.as_bytes());
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_required_spec_claims(&["sub", "scope", "exp"]);
    let data = jsonwebtoken::decode::<Claims>(token, &key, &validation).ok()?;
    if data.claims.scope != "bootstrap" {
        return None;
    }
    Some(AuthContext {
        tenant_id: data.claims.sub.clone(),
        api_key_id: format!("jwt:{}", data.claims.sub),
        scopes: vec!["bootstrap".into()],
        plan: Plan::Starter,
    })
}

// ── API key authentication ───────────────────────────────────────────────────

async fn authenticate_api_key(token: &str, state: &AppState) -> Result<AuthContext, ApiError> {
    // Parse key format: gdpr_sk_{env}_{prefix10}_{suffix}
    let prefix = extract_key_prefix(token);
    let now = now_unix();

    // Check cache
    if let Some(prefix_ref) = prefix {
        if let Some(cached) = state.key_cache.get(prefix_ref) {
            if cached.cached_at + 60 > now {
                // Cache hit, not expired
                return Ok(AuthContext {
                    tenant_id: cached.tenant_id.clone(),
                    api_key_id: cached.api_key_id.clone(),
                    scopes: cached.scopes.clone(),
                    plan: cached.plan.clone(),
                });
            } else {
                // Expired — remove from cache
                drop(cached);
                state.key_cache.remove(prefix_ref);
            }
        }
    }

    // DB lookup: find all non-deleted keys and verify
    let token_owned = token.to_string();
    let conn = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "DB pool error during auth");
        ApiError::Internal(e.to_string())
    })?;

    let row = conn
        .interact(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, name, key_hash, revoked, COALESCE(plan, 'starter') AS plan FROM api_keys",
            )?;
            let rows: Vec<(String, String, String, i32, String)> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i32>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect();

            // Try to verify against each key hash
            for (id, name, key_hash, revoked, plan_str) in &rows {
                use argon2::password_hash::PasswordHash;
                use argon2::{Argon2, PasswordVerifier};
                if let Ok(parsed) = PasswordHash::new(key_hash) {
                    if Argon2::default()
                        .verify_password(token_owned.as_bytes(), &parsed)
                        .is_ok()
                    {
                        return Ok(Some((
                            id.clone(),
                            name.clone(),
                            key_hash.clone(),
                            *revoked,
                            plan_str.clone(),
                        )));
                    }
                }
            }
            Ok::<_, rusqlite::Error>(None)
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "DB interact error during auth");
            ApiError::Internal(e.to_string())
        })?
        .map_err(|e| {
            tracing::error!(error = %e, "DB query error during auth");
            ApiError::Database(e.to_string())
        })?;

    let (id, _name, key_hash, revoked, plan_str) = match row {
        Some(r) => r,
        None => return Err(ApiError::Unauthorized),
    };

    if revoked != 0 {
        return Err(ApiError::KeyRevoked);
    }

    // Build CachedKey and store
    let cached = CachedKey {
        tenant_id: id.clone(),
        api_key_id: id.clone(),
        key_hash,
        scopes: vec!["*".into()],
        plan: parse_plan(&plan_str),
        is_active: true,
        expires_at: None,
        cached_at: now,
    };

    if let Some(prefix_val) = prefix {
        state.key_cache.insert(prefix_val.to_string(), cached);
    }

    Ok(AuthContext {
        tenant_id: id.clone(),
        api_key_id: id,
        scopes: vec!["*".into()],
        plan: parse_plan(&plan_str),
    })
}

/// Extract the 10-char prefix from a key in format `gdpr_sk_{env}_{prefix10}_{suffix}`.
/// Returns None if key doesn't match format.
fn extract_key_prefix(token: &str) -> Option<&str> {
    let rest = token.strip_prefix("gdpr_sk_")?;
    // Skip env segment
    let after_env = rest.find('_').map(|i| &rest[i + 1..])?;
    // Take prefix10 (up to next underscore or end, max 10 chars)
    let end = after_env.find('_').unwrap_or(after_env.len()).min(10);
    if end == 0 {
        return None;
    }
    Some(&after_env[..end])
}

fn parse_plan(s: &str) -> Plan {
    match s {
        "business"   => Plan::Business,
        "enterprise" => Plan::Enterprise,
        _            => Plan::Starter,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jwt_verification_valid() {
        let secret = "test-secret-key-for-jwt";
        let claims = Claims {
            sub: "tenant-123".into(),
            scope: "bootstrap".into(),
            exp: (now_unix() + 3600) as usize,
        };
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let ctx = try_jwt(&token, secret);
        assert!(ctx.is_some());
        let ctx = ctx.unwrap();
        assert_eq!(ctx.tenant_id, "tenant-123");
        assert_eq!(ctx.scopes, vec!["bootstrap".to_string()]);
    }

    #[test]
    fn test_jwt_verification_expired() {
        let secret = "test-secret-key-for-jwt";
        let claims = Claims {
            sub: "tenant-123".into(),
            scope: "bootstrap".into(),
            exp: (now_unix() - 3600) as usize, // expired
        };
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let ctx = try_jwt(&token, secret);
        assert!(ctx.is_none());
    }

    #[test]
    fn test_jwt_wrong_scope() {
        let secret = "test-secret-key-for-jwt";
        let claims = Claims {
            sub: "tenant-123".into(),
            scope: "admin".into(), // not bootstrap
            exp: (now_unix() + 3600) as usize,
        };
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        let ctx = try_jwt(&token, secret);
        assert!(ctx.is_none());
    }

    #[test]
    fn test_extract_key_prefix() {
        assert_eq!(
            extract_key_prefix("gdpr_sk_prod_abcdefghij_suffix123"),
            Some("abcdefghij")
        );
        assert_eq!(extract_key_prefix("not-a-gdpr-key"), None);
        assert_eq!(extract_key_prefix("gdpr_sk_"), None);
    }

    #[test]
    fn test_extract_bearer() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer my-token".parse().unwrap(),
        );
        assert_eq!(extract_bearer(&headers), Some("my-token".to_string()));
    }

    #[test]
    fn test_extract_bearer_missing() {
        let headers = axum::http::HeaderMap::new();
        assert_eq!(extract_bearer(&headers), None);
    }
}
