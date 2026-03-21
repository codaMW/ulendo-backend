use axum::{extract::State, Json};
use serde::Deserialize;
use crate::{auth::AuthUser, error::AppResult, AppState};

pub async fn vapid_public_key(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "publicKey": state.cfg.vapid_public_key }))
}

#[derive(Deserialize)]
pub struct SubscribeRequest {
    pub endpoint:   String,
    pub keys:       SubscriptionKeys,
    pub platform:   Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Deserialize)]
pub struct SubscriptionKeys {
    pub p256dh: String,
    pub auth:   String,
}

pub async fn subscribe(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<SubscribeRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let now = chrono::Utc::now().timestamp();
    let id  = uuid::Uuid::new_v4().to_string().replace('-', "");
    sqlx::query(
        "INSERT OR IGNORE INTO identities (npub, public_key, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4)"
    )
    .bind(&auth.npub).bind(&auth.public_key).bind(now).bind(now)
    .execute(&state.db).await?;

    sqlx::query(
        r#"INSERT INTO push_subscriptions
           (id, npub, endpoint, p256dh, auth, platform, user_agent, created_at, last_used)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
           ON CONFLICT(endpoint) DO UPDATE SET
               npub=?2, p256dh=?4, auth=?5,
               platform=COALESCE(?6,platform),
               user_agent=COALESCE(?7,user_agent),
               last_used=?9"#
    )
    .bind(&id).bind(&auth.npub).bind(&body.endpoint)
    .bind(&body.keys.p256dh).bind(&body.keys.auth)
    .bind(&body.platform).bind(&body.user_agent)
    .bind(now).bind(now)
    .execute(&state.db).await?;

    Ok(Json(serde_json::json!({ "subscribed": true })))
}

#[derive(Deserialize)]
pub struct UnsubscribeRequest {
    pub endpoint: String,
}

pub async fn unsubscribe(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<UnsubscribeRequest>,
) -> AppResult<Json<serde_json::Value>> {
    sqlx::query("DELETE FROM push_subscriptions WHERE endpoint=?1 AND npub=?2")
        .bind(&body.endpoint).bind(&auth.npub)
        .execute(&state.db).await?;
    Ok(Json(serde_json::json!({ "unsubscribed": true })))
}
