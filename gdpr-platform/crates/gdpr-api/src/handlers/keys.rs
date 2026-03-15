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

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}
