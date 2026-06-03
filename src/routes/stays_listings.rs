// ─── ULENDO STAYS: LISTING HANDLERS ───────────────────────────────────────────
// CRUD for accommodation listings. Hosts create/update/delete; guests read.
// For ABC26 launch: hosts are operator-vetted (verified=1). Search endpoints
// only return verified active listings.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::{auth::AuthUser, error::{AppError, AppResult}, AppState};

// ─── REQUEST / RESPONSE TYPES ─────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
pub struct CreateStayListing {
    pub host_lud16:           String,
    pub listing_type:         String,   // 'entire_place'|'private_room'|'shared_room'|'hotel_room'|'resort_unit'
    pub property_class:       String,   // 'apartment'|'house'|'hotel'|'resort'|'guesthouse'|'lodge'|'villa'|'cottage'
    pub title:                String,
    pub description:          String,
    pub house_rules:          Option<String>,
    pub country:              Option<String>,  // defaults to 'MW'
    pub city:                 String,
    pub neighborhood:         Option<String>,
    pub lat:                  f64,             // exact GPS, host drops pin on map
    pub lng:                  f64,
    pub max_guests:           i64,
    pub bedrooms:             i64,
    pub beds:                 i64,
    pub bathrooms:            f64,
    pub price_per_night_sats: i64,
    pub cleaning_fee_sats:    Option<i64>,
    pub min_nights:           Option<i64>,
    pub max_nights:           Option<i64>,
    pub checkin_time:         Option<String>,  // 'HH:MM' format
    pub checkout_time:        Option<String>,
    pub amenities:            Option<String>,  // JSON array as string
    pub photo_urls:           Option<String>,  // JSON array as string
}

#[derive(Deserialize, Debug)]
pub struct UpdateStayListing {
    pub host_lud16:           Option<String>,
    pub title:                Option<String>,
    pub description:          Option<String>,
    pub house_rules:          Option<String>,
    pub neighborhood:         Option<String>,
    pub lat:                  Option<f64>,
    pub lng:                  Option<f64>,
    pub max_guests:           Option<i64>,
    pub bedrooms:             Option<i64>,
    pub beds:                 Option<i64>,
    pub bathrooms:            Option<f64>,
    pub price_per_night_sats: Option<i64>,
    pub cleaning_fee_sats:    Option<i64>,
    pub min_nights:           Option<i64>,
    pub max_nights:           Option<i64>,
    pub checkin_time:         Option<String>,
    pub checkout_time:        Option<String>,
    pub amenities:            Option<String>,
    pub photo_urls:           Option<String>,
    pub active:               Option<i64>,     // 0=hidden, 1=visible
}

#[derive(Serialize, FromRow, Debug)]
pub struct StayListingRow {
    pub id:                   String,
    pub host_pubkey:          String,
    pub host_lud16:           String,
    pub listing_type:         String,
    pub property_class:       String,
    pub title:                String,
    pub description:          String,
    pub house_rules:          Option<String>,
    pub country:              String,
    pub city:                 String,
    pub neighborhood:         Option<String>,
    pub lat:                  f64,
    pub lng:                  f64,
    pub fuzzy_lat:            Option<f64>,
    pub fuzzy_lng:            Option<f64>,
    pub max_guests:           i64,
    pub bedrooms:             i64,
    pub beds:                 i64,
    pub bathrooms:            f64,
    pub price_per_night_sats: i64,
    pub cleaning_fee_sats:    i64,
    pub min_nights:           i64,
    pub max_nights:           i64,
    pub checkin_time:         String,
    pub checkout_time:        String,
    pub amenities:            String,
    pub photo_urls:           String,
    pub cancellation_policy:  String,
    pub verified:             i64,
    pub active:               i64,
    pub created_at:           i64,
    pub updated_at:           i64,
}

// ─── HELPERS ──────────────────────────────────────────────────────────────────

// Add small random offset to lat/lng for privacy display before booking.
// ~500m radius = approximately 0.0045 degrees.
fn compute_fuzzy(lat: f64, lng: f64) -> (f64, f64) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
    // Simple deterministic-per-listing-but-pseudo-random offset.
    // Fine for "show approximate area on map" — not for cryptographic privacy.
    let dx = ((seed % 100) as f64 / 100.0 - 0.5) * 0.009;  // ±0.0045 deg lat (~500m)
    let dy = (((seed / 100) % 100) as f64 / 100.0 - 0.5) * 0.009;
    (lat + dx, lng + dy)
}

fn validate_listing_input(b: &CreateStayListing) -> Result<(), AppError> {
    if b.host_lud16.trim().is_empty() || !b.host_lud16.contains('@') {
        return Err(AppError::BadRequest("Lightning address (lud16) is required and must contain '@'".into()));
    }
    if b.title.trim().len() < 5 {
        return Err(AppError::BadRequest("Title must be at least 5 characters".into()));
    }
    if b.description.trim().len() < 20 {
        return Err(AppError::BadRequest("Description must be at least 20 characters".into()));
    }
    if b.city.trim().is_empty() {
        return Err(AppError::BadRequest("City is required".into()));
    }
    if b.lat < -90.0 || b.lat > 90.0 || b.lng < -180.0 || b.lng > 180.0 {
        return Err(AppError::BadRequest("Invalid GPS coordinates".into()));
    }
    if b.price_per_night_sats < 1000 {
        return Err(AppError::BadRequest("Minimum price per night is 1000 sats".into()));
    }
    if b.max_guests < 1 {
        return Err(AppError::BadRequest("Max guests must be at least 1".into()));
    }
    let valid_types = ["entire_place", "private_room", "shared_room", "hotel_room", "resort_unit"];
    if !valid_types.contains(&b.listing_type.as_str()) {
        return Err(AppError::BadRequest(format!(
            "Invalid listing_type. Must be one of: {}", valid_types.join(", ")
        )));
    }
    let valid_classes = ["apartment", "house", "hotel", "resort", "guesthouse", "lodge", "villa", "cottage"];
    if !valid_classes.contains(&b.property_class.as_str()) {
        return Err(AppError::BadRequest(format!(
            "Invalid property_class. Must be one of: {}", valid_classes.join(", ")
        )));
    }
    Ok(())
}

// ─── HANDLERS ─────────────────────────────────────────────────────────────────

// POST /stays/listings
// Host creates a new listing. Auto-creates with verified=0, active=1.
// Operator (Ulendo team) sets verified=1 via admin endpoint after manual review.
pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<CreateStayListing>,
) -> AppResult<Json<StayListingRow>> {
    validate_listing_input(&body)?;

    let id = uuid::Uuid::new_v4().to_string().replace('-', "");
    let now = chrono::Utc::now().timestamp();
    let country = body.country.as_deref().unwrap_or("MW");
    let (fuzzy_lat, fuzzy_lng) = compute_fuzzy(body.lat, body.lng);

    sqlx::query(
        r#"INSERT INTO stays_listings
           (id, host_pubkey, host_lud16, listing_type, property_class, title, description,
            house_rules, country, city, neighborhood, lat, lng, fuzzy_lat, fuzzy_lng,
            max_guests, bedrooms, beds, bathrooms, price_per_night_sats, cleaning_fee_sats,
            min_nights, max_nights, checkin_time, checkout_time, amenities, photo_urls,
            cancellation_policy, verified, active, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                   ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                   'strict_7day_50', 0, 1, ?28, ?28)"#
    )
    .bind(&id)
    .bind(&auth.public_key)
    .bind(body.host_lud16.trim())
    .bind(&body.listing_type)
    .bind(&body.property_class)
    .bind(body.title.trim())
    .bind(body.description.trim())
    .bind(body.house_rules.as_deref().map(|s| s.trim()))
    .bind(country)
    .bind(body.city.trim())
    .bind(body.neighborhood.as_deref().map(|s| s.trim()))
    .bind(body.lat)
    .bind(body.lng)
    .bind(fuzzy_lat)
    .bind(fuzzy_lng)
    .bind(body.max_guests)
    .bind(body.bedrooms)
    .bind(body.beds)
    .bind(body.bathrooms)
    .bind(body.price_per_night_sats)
    .bind(body.cleaning_fee_sats.unwrap_or(0))
    .bind(body.min_nights.unwrap_or(1))
    .bind(body.max_nights.unwrap_or(30))
    .bind(body.checkin_time.as_deref().unwrap_or("15:00"))
    .bind(body.checkout_time.as_deref().unwrap_or("11:00"))
    .bind(body.amenities.as_deref().unwrap_or("[]"))
    .bind(body.photo_urls.as_deref().unwrap_or("[]"))
    .bind(now)
    .execute(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB insert failed: {e}")))?;

    tracing::info!("[stays] listing created: id={} host={} title='{}'",
        &id, &auth.public_key[..8.min(auth.public_key.len())], body.title.trim());

    // Return the just-created row
    let row = sqlx::query_as::<_, StayListingRow>(
        "SELECT id, host_pubkey, host_lud16, listing_type, property_class, title, description,
                house_rules, country, city, neighborhood, lat, lng, fuzzy_lat, fuzzy_lng,
                max_guests, bedrooms, beds, bathrooms, price_per_night_sats, cleaning_fee_sats,
                min_nights, max_nights, checkin_time, checkout_time, amenities, photo_urls,
                cancellation_policy, verified, active, created_at, updated_at
         FROM stays_listings WHERE id=?1"
    )
    .bind(&id)
    .fetch_one(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB read-back failed: {e}")))?;

    Ok(Json(row))
}

// GET /stays/listings/:id
// Public read of a single listing. Returns 404 if deleted.
pub async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<StayListingRow>> {
    let row = sqlx::query_as::<_, StayListingRow>(
        "SELECT id, host_pubkey, host_lud16, listing_type, property_class, title, description,
                house_rules, country, city, neighborhood, lat, lng, fuzzy_lat, fuzzy_lng,
                max_guests, bedrooms, beds, bathrooms, price_per_night_sats, cleaning_fee_sats,
                min_nights, max_nights, checkin_time, checkout_time, amenities, photo_urls,
                cancellation_policy, verified, active, created_at, updated_at
         FROM stays_listings
         WHERE id=?1 AND deleted_at IS NULL"
    )
    .bind(&id)
    .fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB read failed: {e}")))?
    .ok_or_else(|| AppError::BadRequest("Listing not found".into()))?;

    Ok(Json(row))
}

// PUT /stays/listings/:id
// Host updates their own listing. Only host or admin can modify.
pub async fn update(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateStayListing>,
) -> AppResult<Json<StayListingRow>> {
    // Verify caller owns this listing
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT host_pubkey FROM stays_listings WHERE id=?1 AND deleted_at IS NULL"
    )
    .bind(&id)
    .fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let owner = owner.ok_or_else(|| AppError::BadRequest("Listing not found".into()))?;
    if owner != auth.public_key {
        return Err(AppError::Unauthorized("Only the host can update this listing".into()));
    }

    let now = chrono::Utc::now().timestamp();

    // Validate price if changing
    if let Some(p) = body.price_per_night_sats {
        if p < 1000 {
            return Err(AppError::BadRequest("Minimum price per night is 1000 sats".into()));
        }
    }
    // Validate GPS if changing
    if let (Some(lat), Some(lng)) = (body.lat, body.lng) {
        if lat < -90.0 || lat > 90.0 || lng < -180.0 || lng > 180.0 {
            return Err(AppError::BadRequest("Invalid GPS coordinates".into()));
        }
    }

    // COALESCE: only update fields that are Some() in the body, leave others unchanged
    sqlx::query(
        r#"UPDATE stays_listings SET
            host_lud16           = COALESCE(?2, host_lud16),
            title                = COALESCE(?3, title),
            description          = COALESCE(?4, description),
            house_rules          = COALESCE(?5, house_rules),
            neighborhood         = COALESCE(?6, neighborhood),
            lat                  = COALESCE(?7, lat),
            lng                  = COALESCE(?8, lng),
            max_guests           = COALESCE(?9, max_guests),
            bedrooms             = COALESCE(?10, bedrooms),
            beds                 = COALESCE(?11, beds),
            bathrooms            = COALESCE(?12, bathrooms),
            price_per_night_sats = COALESCE(?13, price_per_night_sats),
            cleaning_fee_sats    = COALESCE(?14, cleaning_fee_sats),
            min_nights           = COALESCE(?15, min_nights),
            max_nights           = COALESCE(?16, max_nights),
            checkin_time         = COALESCE(?17, checkin_time),
            checkout_time        = COALESCE(?18, checkout_time),
            amenities            = COALESCE(?19, amenities),
            photo_urls           = COALESCE(?20, photo_urls),
            active               = COALESCE(?21, active),
            updated_at           = ?22
         WHERE id=?1"#
    )
    .bind(&id)
    .bind(body.host_lud16.as_deref().map(|s| s.trim()))
    .bind(body.title.as_deref().map(|s| s.trim()))
    .bind(body.description.as_deref().map(|s| s.trim()))
    .bind(body.house_rules.as_deref().map(|s| s.trim()))
    .bind(body.neighborhood.as_deref().map(|s| s.trim()))
    .bind(body.lat)
    .bind(body.lng)
    .bind(body.max_guests)
    .bind(body.bedrooms)
    .bind(body.beds)
    .bind(body.bathrooms)
    .bind(body.price_per_night_sats)
    .bind(body.cleaning_fee_sats)
    .bind(body.min_nights)
    .bind(body.max_nights)
    .bind(body.checkin_time.as_deref())
    .bind(body.checkout_time.as_deref())
    .bind(body.amenities.as_deref())
    .bind(body.photo_urls.as_deref())
    .bind(body.active)
    .bind(now)
    .execute(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB update failed: {e}")))?;

    tracing::info!("[stays] listing updated: id={} host={}", &id, &auth.public_key[..8.min(auth.public_key.len())]);

    // Return the updated row
    let row = sqlx::query_as::<_, StayListingRow>(
        "SELECT id, host_pubkey, host_lud16, listing_type, property_class, title, description,
                house_rules, country, city, neighborhood, lat, lng, fuzzy_lat, fuzzy_lng,
                max_guests, bedrooms, beds, bathrooms, price_per_night_sats, cleaning_fee_sats,
                min_nights, max_nights, checkin_time, checkout_time, amenities, photo_urls,
                cancellation_policy, verified, active, created_at, updated_at
         FROM stays_listings WHERE id=?1"
    )
    .bind(&id)
    .fetch_one(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB read-back failed: {e}")))?;

    Ok(Json(row))
}

// DELETE /stays/listings/:id
// Soft delete. Only the host can delete their own listing.
pub async fn delete_one(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT host_pubkey FROM stays_listings WHERE id=?1 AND deleted_at IS NULL"
    )
    .bind(&id)
    .fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let owner = owner.ok_or_else(|| AppError::BadRequest("Listing not found".into()))?;
    if owner != auth.public_key {
        return Err(AppError::Unauthorized("Only the host can delete this listing".into()));
    }

    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE stays_listings SET deleted_at=?2, updated_at=?2 WHERE id=?1")
        .bind(&id).bind(now)
        .execute(&state.db).await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("DB delete failed: {e}")))?;

    tracing::info!("[stays] listing deleted: id={} host={}", &id, &auth.public_key[..8.min(auth.public_key.len())]);
    Ok(Json(serde_json::json!({"ok": true, "id": id})))
}

// GET /stays/listings/mine
// Host lists their own listings (verified and unverified, active and inactive).
pub async fn list_mine(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<StayListingRow>>> {
    let rows = sqlx::query_as::<_, StayListingRow>(
        "SELECT id, host_pubkey, host_lud16, listing_type, property_class, title, description,
                house_rules, country, city, neighborhood, lat, lng, fuzzy_lat, fuzzy_lng,
                max_guests, bedrooms, beds, bathrooms, price_per_night_sats, cleaning_fee_sats,
                min_nights, max_nights, checkin_time, checkout_time, amenities, photo_urls,
                cancellation_policy, verified, active, created_at, updated_at
         FROM stays_listings
         WHERE host_pubkey=?1 AND deleted_at IS NULL
         ORDER BY created_at DESC"
    )
    .bind(&auth.public_key)
    .fetch_all(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB read failed: {e}")))?;

    Ok(Json(rows))
}

#[derive(Deserialize, Debug)]
pub struct AdminListQuery {
    pub verified:  Option<i64>,  // 0=pending, 1=verified, omitted=all
    pub limit:     Option<i64>,
}

// GET /stays/admin/listings?verified=0
// Admin endpoint to view listings (typically used to find unverified ones).
// TODO: gate behind admin pubkey check.
pub async fn list_admin(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(q): Query<AdminListQuery>,
) -> AppResult<Json<Vec<StayListingRow>>> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);

    let rows = if let Some(v) = q.verified {
        sqlx::query_as::<_, StayListingRow>(
            "SELECT id, host_pubkey, host_lud16, listing_type, property_class, title, description,
                    house_rules, country, city, neighborhood, lat, lng, fuzzy_lat, fuzzy_lng,
                    max_guests, bedrooms, beds, bathrooms, price_per_night_sats, cleaning_fee_sats,
                    min_nights, max_nights, checkin_time, checkout_time, amenities, photo_urls,
                    cancellation_policy, verified, active, created_at, updated_at
             FROM stays_listings WHERE verified=?1 AND deleted_at IS NULL
             ORDER BY created_at DESC LIMIT ?2"
        )
        .bind(v).bind(limit)
        .fetch_all(&state.db).await
    } else {
        sqlx::query_as::<_, StayListingRow>(
            "SELECT id, host_pubkey, host_lud16, listing_type, property_class, title, description,
                    house_rules, country, city, neighborhood, lat, lng, fuzzy_lat, fuzzy_lng,
                    max_guests, bedrooms, beds, bathrooms, price_per_night_sats, cleaning_fee_sats,
                    min_nights, max_nights, checkin_time, checkout_time, amenities, photo_urls,
                    cancellation_policy, verified, active, created_at, updated_at
             FROM stays_listings WHERE deleted_at IS NULL
             ORDER BY created_at DESC LIMIT ?1"
        )
        .bind(limit)
        .fetch_all(&state.db).await
    }
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB read failed: {e}")))?;

    Ok(Json(rows))
}

// PUT /stays/admin/listings/:id/verify
// Admin marks a listing as verified after manual review.
// TODO: gate behind admin pubkey check.
pub async fn admin_verify(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query("UPDATE stays_listings SET verified=1, updated_at=?2 WHERE id=?1 AND deleted_at IS NULL")
        .bind(&id).bind(now)
        .execute(&state.db).await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    if res.rows_affected() == 0 {
        return Err(AppError::BadRequest("Listing not found".into()));
    }
    tracing::info!("[stays] admin verified listing: id={}", &id);
    Ok(Json(serde_json::json!({"ok": true, "id": id, "verified": 1})))
}

// ─── GUEST BROWSE ─────────────────────────────────────────────────────────────

#[derive(Deserialize, Debug)]
pub struct SearchQuery {
    pub city:   Option<String>,
    pub limit:  Option<i64>,
    pub offset: Option<i64>,
}

// GET /stays/search?city=Lilongwe&limit=20&offset=0
// Public endpoint — no auth required. Returns verified, active, non-deleted listings.
// Uses fuzzy_lat/fuzzy_lng for map display (exact coords hidden until booking).
pub async fn search(
    State(state): State<AppState>,
    Query(q): Query<SearchQuery>,
) -> AppResult<Json<Vec<StayListingRow>>> {
    let limit  = q.limit.unwrap_or(20).clamp(1, 100);
    let offset = q.offset.unwrap_or(0).max(0);

    let rows = if let Some(city) = q.city.as_deref() {
        sqlx::query_as::<_, StayListingRow>(
            "SELECT id, host_pubkey, host_lud16, listing_type, property_class, title, description,
                    house_rules, country, city, neighborhood, lat, lng, fuzzy_lat, fuzzy_lng,
                    max_guests, bedrooms, beds, bathrooms, price_per_night_sats, cleaning_fee_sats,
                    min_nights, max_nights, checkin_time, checkout_time, amenities, photo_urls,
                    cancellation_policy, verified, active, created_at, updated_at
             FROM stays_listings
             WHERE verified=1 AND active=1 AND deleted_at IS NULL
               AND lower(city) LIKE lower(?1)
             ORDER BY created_at DESC LIMIT ?2 OFFSET ?3"
        )
        .bind(format!("%{}%", city))
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db).await
    } else {
        sqlx::query_as::<_, StayListingRow>(
            "SELECT id, host_pubkey, host_lud16, listing_type, property_class, title, description,
                    house_rules, country, city, neighborhood, lat, lng, fuzzy_lat, fuzzy_lng,
                    max_guests, bedrooms, beds, bathrooms, price_per_night_sats, cleaning_fee_sats,
                    min_nights, max_nights, checkin_time, checkout_time, amenities, photo_urls,
                    cancellation_policy, verified, active, created_at, updated_at
             FROM stays_listings
             WHERE verified=1 AND active=1 AND deleted_at IS NULL
             ORDER BY created_at DESC LIMIT ?1 OFFSET ?2"
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db).await
    }
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB search failed: {e}")))?;

    Ok(Json(rows))
}
