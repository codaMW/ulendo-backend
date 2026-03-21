use axum::{extract::{Path, State}, Json};
use serde::Deserialize;
use crate::{auth::AuthUser, db::Identity, error::AppResult, AppState};

#[derive(Deserialize)]
pub struct UpsertIdentityRequest {
    pub name:  Option<String>,
    pub role:  Option<String>,
    pub lud16: Option<String>,
}

pub async fn upsert(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<UpsertIdentityRequest>,
) -> AppResult<Json<Identity>> {
    let role = body.role.as_deref().unwrap_or("visitor");
    if !["visitor","merchant"].contains(&role) {
        return Err(crate::AppError::BadRequest("role must be visitor or merchant".into()));
    }
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"INSERT INTO identities (npub, public_key, name, role, lud16, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
           ON CONFLICT(npub) DO UPDATE SET
               name       = COALESCE(?3, name),
               role       = COALESCE(?4, role),
               lud16      = COALESCE(?5, lud16),
               updated_at = ?7"#
    )
    .bind(&auth.npub)
    .bind(&auth.public_key)
    .bind(&body.name)
    .bind(role)
    .bind(&body.lud16)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;

    let identity = sqlx::query_as::<_, Identity>(
        "SELECT * FROM identities WHERE npub = ?1"
    )
    .bind(&auth.npub)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(identity))
}

pub async fn get_by_npub(
    Path(npub): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<Identity>> {
    let identity = sqlx::query_as::<_, Identity>(
        "SELECT * FROM identities WHERE npub = ?1"
    )
    .bind(&npub)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| crate::AppError::NotFound(format!("identity {npub} not found")))?;

    Ok(Json(identity))
}