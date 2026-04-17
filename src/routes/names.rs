use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::{auth::AuthUser, error::AppResult, AppState};

// ── query params for /.well-known/nostr.json ─────────────────────────────────
#[derive(Deserialize)]
pub struct Nip05Query {
    pub name: Option<String>,
}

// ── GET /.well-known/nostr.json?name=yankho ──────────────────────────────────
// Spec: https://github.com/nostr-protocol/nips/blob/master/05.md
// Returns { "names": { "yankho": "<hex_pubkey>" }, "relays": { "<hex>": [...] } }
// If no `name` param → returns ALL names (useful for directory lookups)
pub async fn well_known_nostr_json(
    State(state): State<AppState>,
    Query(params): Query<Nip05Query>,
) -> Response {
    let db = &state.db;

    // CORS header is mandatory — pure-JS clients require it
    let cors = [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")];

    match params.name {
        Some(ref name) => {
            let name_lower = name.to_lowercase();
            // Support "_" as the root identifier (NIP-05 spec)
            let lookup = if name_lower == "_" { "_".to_string() } else { name_lower };

            let row = sqlx::query!(
                "SELECT pubkey_hex, relays_json FROM nostr_names WHERE username = ?",
                lookup
            )
            .fetch_optional(db)
            .await;

            match row {
                Ok(Some(r)) => {
                    let relays: Vec<String> = serde_json::from_str(&r.relays_json)
                        .unwrap_or_default();
                    let mut relay_map: HashMap<String, Vec<String>> = HashMap::new();
                    if !relays.is_empty() {
                        relay_map.insert(r.pubkey_hex.clone(), relays);
                    }
                    let body = json!({
                        "names": { lookup: r.pubkey_hex },
                        "relays": relay_map,
                    });
                    (StatusCode::OK, cors, Json(body)).into_response()
                }
                _ => (StatusCode::NOT_FOUND, cors, Json(json!({ "names": {} }))).into_response(),
            }
        }
        // No name param → return full directory (for client-side search)
        None => {
            let rows = sqlx::query!("SELECT username, pubkey_hex FROM nostr_names ORDER BY created_at DESC LIMIT 1000")
                .fetch_all(db)
                .await
                .unwrap_or_default();

            let mut names: HashMap<String, String> = HashMap::new();
            for r in rows {
                names.insert(r.username.clone().unwrap_or_default(), r.pubkey_hex.clone());
            }
            let body = json!({ "names": names });
            (StatusCode::OK, cors, Json(body)).into_response()
        }
    }
}

// ── request / response types ──────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct RegisterNameRequest {
    pub username: String,
    pub pubkey_hex: String,
    pub relays: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct RegisterNameResponse {
    pub username: String,
    pub nip05: String,
    pub lud16: String,
}

#[derive(Deserialize)]
pub struct CheckNameQuery {
    pub username: String,
}

// ── GET /names/check?username=yankho ─────────────────────────────────────────
pub async fn check_username(
    State(state): State<AppState>,
    Query(params): Query<CheckNameQuery>,
) -> AppResult<Json<Value>> {
    let username = params.username.to_lowercase();

    if !is_valid_username(&username) {
        return Ok(Json(json!({
            "available": false,
            "reason": "Username must be 2-30 chars, only letters, numbers, _ . -"
        })));
    }

    let taken = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM nostr_names WHERE username = ?",
        username
    )
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "available": taken == 0,
        "username": username,
        "nip05": format!("{}@ulendo.app", username),
        "lud16": format!("{}@ulendo.app", username),
    })))
}

// ── POST /names/register ──────────────────────────────────────────────────────
// Authenticated — user must have a valid NIP-98 auth header
pub async fn register_name(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(req): Json<RegisterNameRequest>,
) -> AppResult<Json<RegisterNameResponse>> {
    let username = req.username.to_lowercase();

    if !is_valid_username(&username) {
        return Err(crate::error::AppError::BadRequest(
            "Invalid username: 2-30 chars, a-z 0-9 _ . - only".into(),
        ));
    }

    // Verify the pubkey in the request matches the authenticated user
    if req.pubkey_hex != auth.public_key {
        return Err(crate::error::AppError::Unauthorized(
            "pubkey_hex must match your authenticated key".into(),
        ));
    }

    let relays_json = serde_json::to_string(&req.relays.unwrap_or_default())
        .unwrap_or_else(|_| "[]".to_string());

    // Upsert — allow user to update their own name registration
    // But block if the username is taken by someone else
    let existing = sqlx::query!(
        "SELECT pubkey_hex FROM nostr_names WHERE username = ?",
        username
    )
    .fetch_optional(&state.db)
    .await?;

    if let Some(row) = existing {
        if row.pubkey_hex != auth.public_key {
            return Err(crate::error::AppError::Conflict(
                "Username already taken".into(),
            ));
        }
    }

    sqlx::query!(
        r#"
        INSERT INTO nostr_names (username, pubkey_hex, relays_json, updated_at)
        VALUES (?, ?, ?, unixepoch())
        ON CONFLICT(username) DO UPDATE SET
            relays_json = excluded.relays_json,
            updated_at  = unixepoch()
        "#,
        username,
        auth.public_key,
        relays_json,
    )
    .execute(&state.db)
    .await?;

    Ok(Json(RegisterNameResponse {
        nip05: format!("{}@ulendo.app", username),
        lud16: format!("{}@ulendo.app", username),
        username,
    }))
}

// ── GET /names/by-pubkey/:pubkey ──────────────────────────────────────────────
pub async fn get_name_by_pubkey(
    State(state): State<AppState>,
    axum::extract::Path(pubkey): axum::extract::Path<String>,
) -> AppResult<Json<Value>> {
    let row = sqlx::query!(
        "SELECT username FROM nostr_names WHERE pubkey_hex = ? LIMIT 1",
        pubkey
    )
    .fetch_optional(&state.db)
    .await?;

    match row {
        Some(r) => Ok(Json(json!({
            "username": r.username,
            "nip05": format!("{}@ulendo.app", r.username.as_deref().unwrap_or("unknown")),
            "lud16": format!("{}@ulendo.app", r.username.as_deref().unwrap_or("unknown")),
        }))),
        None => Ok(Json(json!({ "username": null }))),
    }
}

// ── validation helper ─────────────────────────────────────────────────────────
fn is_valid_username(s: &str) -> bool {
    if s.len() < 2 || s.len() > 30 {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.' || c == '-')
}
