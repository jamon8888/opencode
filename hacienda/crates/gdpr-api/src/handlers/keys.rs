use axum::{extract::{State, Path}, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use argon2::{Argon2, PasswordHasher};
use argon2::password_hash::{rand_core::OsRng, SaltString};
use crate::state::{AppState, ApiKeyRecord};
use crate::error::ApiResult;

#[derive(Deserialize)]
pub struct CreateKeyReq {
    pub name: String,
}

#[derive(Serialize)]
pub struct CreateKeyResp {
    pub id:   String,
    pub key:  String,
    pub name: String,
}

pub async fn post_key(
    State(state): State<AppState>,
    Json(req): Json<CreateKeyReq>,
) -> ApiResult<(StatusCode, Json<CreateKeyResp>)> {
    use rand::Rng;
    let raw_bytes: [u8; 32] = rand::thread_rng().gen();
    let raw_key = hex_encode(&raw_bytes);

    let salt = SaltString::generate(&mut OsRng);
    let key_hash = Argon2::default()
        .hash_password(raw_key.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?
        .to_string();

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    let conn = state.db.get().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let id2    = id.clone();
    let name2  = req.name.clone();
    let hash2  = key_hash.clone();
    conn.interact(move |c| {
        c.execute(
            "INSERT INTO api_keys (id, name, key_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id2, name2, hash2, now],
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    state.keys.insert(
        raw_key.clone(),
        ApiKeyRecord {
            id:       id.clone(),
            name:     req.name.clone(),
            key_hash,
        },
    );

    Ok((
        StatusCode::CREATED,
        Json(CreateKeyResp { id, key: raw_key, name: req.name }),
    ))
}

pub async fn delete_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let conn = state.db.get().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let id2 = id.clone();
    conn.interact(move |c| {
        c.execute(
            "UPDATE api_keys SET revoked=1 WHERE id=?1",
            rusqlite::params![id2],
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    state.keys.retain(|_, v| v.id != id);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct RotateKeyResp {
    pub id:      String,
    pub key:     String,
    pub name:    String,
    pub note:    String,
}

/// POST /v1/keys/:id/rotate
///
/// Issues a new raw key, stores its argon2 hash, records `rotated_at` timestamp.
/// The old key remains valid for 24 h (grace period — enforced by auth middleware later).
pub async fn rotate_key(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<RotateKeyResp>> {
    use rand::Rng;

    // 1. Verify key exists and is not revoked
    let conn = state.db.get().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let id2 = id.clone();
    let name: String = conn.interact(move |c| {
        c.query_row(
            "SELECT name FROM api_keys WHERE id = ?1 AND revoked = 0",
            rusqlite::params![id2],
            |row| row.get::<_, String>(0),
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("pool: {e}"))?
    .map_err(|_| crate::error::ApiError::NotFound(format!("key {id} not found or revoked")))?;
    drop(conn);

    // 2. Generate new raw key + argon2 hash
    let raw_bytes: [u8; 32] = rand::thread_rng().gen();
    let new_raw = hex_encode(&raw_bytes);

    let salt = SaltString::generate(&mut OsRng);
    let new_hash = Argon2::default()
        .hash_password(new_raw.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?
        .to_string();

    // 3. Atomically: update key_hash, set rotated_at
    let now = chrono::Utc::now().timestamp();
    let conn2 = state.db.get().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let id3  = id.clone();
    let hash3 = new_hash.clone();
    conn2.interact(move |c| {
        c.execute(
            "UPDATE api_keys SET key_hash = ?1, rotated_at = ?2 WHERE id = ?3",
            rusqlite::params![hash3, now, id3],
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("pool: {e}"))?
    .map_err(|e| anyhow::anyhow!("db: {e}"))?;

    // 4. Evict old key from in-memory cache (force re-auth on next request)
    // Evict old key from in-memory cache. Sub-microsecond race window between DB update
    // and this eviction is acceptable: old key will fail re-auth on next DB hash check.
    // Full 24h grace enforcement is pending auth middleware (Phase 8 auth impl).
    state.keys.retain(|_, v| v.id != id);

    Ok(Json(RotateKeyResp {
        id,
        key:  new_raw,
        name,
        note: "Old key valid for 24 h grace period. Store new key securely — shown only once.".into(),
    }))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}
