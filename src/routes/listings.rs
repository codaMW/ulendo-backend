use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use crate::{auth::AuthUser, db::Listing, error::{AppError, AppResult}, AppState};

#[derive(Deserialize)]
pub struct ListQuery {
    pub category: Option<String>,
    pub area:     Option<String>,
    pub limit:    Option<i64>,
    pub offset:   Option<i64>,
}

#[derive(Serialize)]
pub struct ListingsResponse {
    pub items:  Vec<Listing>,
    pub total:  i64,
    pub limit:  i64,
    pub offset: i64,
}

pub async fn list(
    Query(q): Query<ListQuery>,
    State(state): State<AppState>,
) -> AppResult<Json<ListingsResponse>> {
    let limit  = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);

    let items = sqlx::query_as::<_, Listing>(
        r#"SELECT * FROM listings
           WHERE available = 1
             AND (?1 IS NULL OR category = ?1)
             AND (?2 IS NULL OR area LIKE '%' || ?2 || '%')
           ORDER BY created_at DESC
           LIMIT ?3 OFFSET ?4"#
    )
    .bind(&q.category)
    .bind(&q.area)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let total: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM listings
           WHERE available = 1
             AND (?1 IS NULL OR category = ?1)
             AND (?2 IS NULL OR area LIKE '%' || ?2 || '%')"#
    )
    .bind(&q.category)
    .bind(&q.area)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(ListingsResponse { items, total, limit, offset }))
}

pub async fn get_one(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<Listing>> {
    let listing = sqlx::query_as::<_, Listing>("SELECT * FROM listings WHERE id = ?1")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("listing {id} not found")))?;
    Ok(Json(listing))
}

#[derive(Deserialize)]
pub struct CreateListingRequest {
    pub nostr_event_id: Option<String>,
    pub category:       String,
    pub name:           String,
    pub area:           String,
    pub description:    Option<String>,
    pub price_sats:     i64,
    pub price_unit:     Option<String>,
    pub lud16:          Option<String>,
    pub photos:         Option<Vec<String>>,
    pub phone:          Option<String>,
}

pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<CreateListingRequest>,
) -> AppResult<Json<Listing>> {
    // Ensure identity exists
    ensure_identity_exists(&state, &auth).await?;

    let valid_categories = ["guide","transport","stay","restaurant"];
    if !valid_categories.contains(&body.category.as_str()) {
        return Err(AppError::BadRequest("invalid category".into()));
    }

    let photos_json = serde_json::to_string(&body.photos.unwrap_or_default())
        .unwrap_or_else(|_| "[]".into());
    let price_unit  = body.price_unit.unwrap_or_else(|| "per day".into());
    let now         = chrono::Utc::now().timestamp();
    let id          = uuid::Uuid::new_v4().to_string().replace('-', "");

    sqlx::query(
        r#"INSERT INTO listings
           (id, owner_npub, nostr_event_id, category, name, area, description,
            price_sats, price_unit, lud16, photos_json, phone, verified, created_at, updated_at)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,1,?13,?14)"#
    )
    .bind(&id)
    .bind(&auth.npub)
    .bind(&body.nostr_event_id)
    .bind(&body.category)
    .bind(&body.name)
    .bind(&body.area)
    .bind(&body.description)
    .bind(body.price_sats)
    .bind(&price_unit)
    .bind(&body.lud16)
    .bind(&photos_json)
    .bind(&body.phone)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;

    let listing = sqlx::query_as::<_, Listing>("SELECT * FROM listings WHERE id = ?1")
        .bind(&id)
        .fetch_one(&state.db)
        .await?;

    Ok(Json(listing))
}

#[derive(Deserialize)]
pub struct UpdateListingRequest {
    pub name:        Option<String>,
    pub description: Option<String>,
    pub price_sats:  Option<i64>,
    pub lud16:       Option<String>,
    pub available:   Option<bool>,
    pub photos:      Option<Vec<String>>,
}

pub async fn update(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<UpdateListingRequest>,
) -> AppResult<Json<Listing>> {
    // Verify ownership
    let listing = sqlx::query_as::<_, Listing>("SELECT * FROM listings WHERE id = ?1")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("listing {id} not found")))?;

    if listing.owner_npub != auth.npub {
        return Err(AppError::Unauthorized("you don't own this listing".into()));
    }

    let now         = chrono::Utc::now().timestamp();
    let photos_json = body.photos.map(|p| serde_json::to_string(&p).unwrap_or_default());
    let available   = body.available.map(|a| if a { 1i64 } else { 0i64 });

    sqlx::query(
        r#"UPDATE listings SET
           name        = COALESCE(?1, name),
           description = COALESCE(?2, description),
           price_sats  = COALESCE(?3, price_sats),
           lud16       = COALESCE(?4, lud16),
           available   = COALESCE(?5, available),
           photos_json = COALESCE(?6, photos_json),
           updated_at  = ?7
           WHERE id = ?8"#
    )
    .bind(&body.name)
    .bind(&body.description)
    .bind(body.price_sats)
    .bind(&body.lud16)
    .bind(available)
    .bind(&photos_json)
    .bind(now)
    .bind(&id)
    .execute(&state.db)
    .await?;

    let updated = sqlx::query_as::<_, Listing>("SELECT * FROM listings WHERE id = ?1")
        .bind(&id)
        .fetch_one(&state.db)
        .await?;

    Ok(Json(updated))
}

pub async fn remove(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let listing = sqlx::query_as::<_, Listing>("SELECT * FROM listings WHERE id = ?1")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("listing {id} not found")))?;

    if listing.owner_npub != auth.npub {
        return Err(AppError::Unauthorized("you don't own this listing".into()));
    }

    // Soft delete — mark unavailable rather than hard delete
    // (active bookings reference this row)
    sqlx::query("UPDATE listings SET available = 0, updated_at = ?1 WHERE id = ?2")
        .bind(chrono::Utc::now().timestamp())
        .bind(&id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({ "deleted": true, "id": id })))
}

async fn ensure_identity_exists(state: &AppState, auth: &AuthUser) -> AppResult<()> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM identities WHERE npub=?1)")
        .bind(&auth.npub)
        .fetch_one(&state.db)
        .await?;

    if !exists {
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT OR IGNORE INTO identities (npub, public_key, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4)"
        )
        .bind(&auth.npub)
        .bind(&auth.public_key)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await?;
    }
    Ok(())
}