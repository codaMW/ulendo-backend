use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use crate::{auth::AuthUser, error::{AppError, AppResult}, AppState};

const MAX_LISTINGS_PER_DRIVER: i64 = 5;

#[derive(Deserialize, Debug)]
pub struct UpsertDriverListing {
    pub id:                  String,
    pub listing_name:        Option<String>,
    pub vehicle:             Option<String>,
    pub vehicle_type:        Option<String>,
    pub seats:               Option<i64>,
    pub price_per_km:        Option<i64>,
    pub ride_categories:     Option<String>,
    pub photo_urls:          Option<String>,
    pub description:         Option<String>,
    pub location_country:    Option<String>,
    pub location_city:       Option<String>,
    pub lud16:               Option<String>,
    pub nostr_event_id:      Option<String>,
    pub service_interval_km: Option<i64>,   // default 5000 if not set
}

#[derive(Serialize, Deserialize, FromRow, Debug)]
pub struct DriverListingRow {
    pub id:                  String,
    pub driver_pubkey:       String,
    pub driver_npub:         Option<String>,
    pub listing_name:        Option<String>,
    pub vehicle:             Option<String>,
    pub vehicle_type:        Option<String>,
    pub seats:               Option<i64>,
    pub price_per_km:        Option<i64>,
    pub ride_categories:     Option<String>,
    pub photo_urls:          Option<String>,
    pub description:         Option<String>,
    pub location_country:    Option<String>,
    pub location_city:       Option<String>,
    pub lud16:               Option<String>,
    pub nostr_event_id:      Option<String>,
    pub service_interval_km: i64,
    pub km_driven_total:     i64,
    pub created_at:          i64,
    pub updated_at:          i64,
}

/// POST /listings/driver
/// Create or update a driver listing. UPSERT by id.
/// Enforces a 5-listing cap per driver (counting only non-deleted listings).
pub async fn upsert(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<UpsertDriverListing>,
) -> AppResult<Json<DriverListingRow>> {
    // PHASE 2 FIX: enforce minimum price_per_km so drivers can't list rides that would
    // produce sub-Lightning-minimum fares.
    let min_ppk = state.cfg.min_price_per_km_sats;
    if let Some(ppk) = body.price_per_km {
        if ppk < min_ppk {
            return Err(AppError::BadRequest(format!(
                "Minimum price per km is {} sats (you set {}).", min_ppk, ppk
            )));
        }
    }
    let now = chrono::Utc::now().timestamp();

    // Check if this listing already exists (so cap check only counts new creations)
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM driver_listings WHERE id = ?1 AND driver_pubkey = ?2 AND deleted_at IS NULL"
    )
    .bind(&body.id).bind(&auth.public_key)
    .fetch_optional(&state.db).await?;

    if existing.is_none() {
        // Creating a new listing — enforce cap
        let (active_count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM driver_listings WHERE driver_pubkey = ?1 AND deleted_at IS NULL"
        )
        .bind(&auth.public_key)
        .fetch_one(&state.db).await?;
        if active_count >= MAX_LISTINGS_PER_DRIVER {
            return Err(AppError::BadRequest(format!(
                "max {} active listings per driver — delete one before creating a new one",
                MAX_LISTINGS_PER_DRIVER
            )));
        }
    }

    sqlx::query(
        r#"INSERT INTO driver_listings
           (id, driver_pubkey, driver_npub, listing_name, vehicle, vehicle_type, seats, price_per_km,
            ride_categories, photo_urls, description, location_country, location_city, lud16,
            nostr_event_id, service_interval_km, created_at, updated_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?17)
           ON CONFLICT(id) DO UPDATE SET
             listing_name        = ?4,
             vehicle             = ?5,
             vehicle_type        = ?6,
             seats               = ?7,
             price_per_km        = ?8,
             ride_categories     = ?9,
             photo_urls          = ?10,
             description         = ?11,
             location_country    = ?12,
             location_city       = ?13,
             lud16               = ?14,
             nostr_event_id      = ?15,
             service_interval_km = ?16,
             updated_at          = ?17,
             deleted_at          = NULL"#
    )
    .bind(&body.id)
    .bind(&auth.public_key)
    .bind(&auth.npub)
    .bind(&body.listing_name)
    .bind(&body.vehicle)
    .bind(&body.vehicle_type)
    .bind(body.seats.unwrap_or(4))
    .bind(body.price_per_km.unwrap_or(500))
    .bind(&body.ride_categories)
    .bind(&body.photo_urls)
    .bind(&body.description)
    .bind(&body.location_country)
    .bind(&body.location_city)
    .bind(&body.lud16)
    .bind(&body.nostr_event_id)
    .bind(body.service_interval_km.unwrap_or(5000))
    .bind(now)
    .execute(&state.db).await?;

    let row: DriverListingRow = sqlx::query_as(
        "SELECT id, driver_pubkey, driver_npub, listing_name, vehicle, vehicle_type, seats,
                price_per_km, ride_categories, photo_urls, description, location_country,
                location_city, lud16, nostr_event_id, service_interval_km, km_driven_total,
                created_at, updated_at
         FROM driver_listings WHERE id = ?1"
    )
    .bind(&body.id)
    .fetch_one(&state.db).await?;

    Ok(Json(row))
}

/// GET /listings/driver/mine
/// Returns all of the calling driver's active (non-deleted) listings.
pub async fn list_mine(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<DriverListingRow>>> {
    let rows: Vec<DriverListingRow> = sqlx::query_as(
        "SELECT id, driver_pubkey, driver_npub, listing_name, vehicle, vehicle_type, seats,
                price_per_km, ride_categories, photo_urls, description, location_country,
                location_city, lud16, nostr_event_id, service_interval_km, km_driven_total,
                created_at, updated_at
         FROM driver_listings
         WHERE driver_pubkey = ?1 AND deleted_at IS NULL
         ORDER BY updated_at DESC"
    )
    .bind(&auth.public_key)
    .fetch_all(&state.db).await?;
    Ok(Json(rows))
}

/// DELETE /listings/driver/:id
/// Soft delete. Only the listing's owner can delete it.
pub async fn delete_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let now = chrono::Utc::now().timestamp();
    let result = sqlx::query(
        "UPDATE driver_listings
         SET deleted_at = ?1, updated_at = ?1
         WHERE id = ?2 AND driver_pubkey = ?3 AND deleted_at IS NULL"
    )
    .bind(now).bind(&id).bind(&auth.public_key)
    .execute(&state.db).await?;

    if result.rows_affected() == 0 {
        return Err(AppError::BadRequest("listing not found or not owned by you".into()));
    }
    Ok(Json(serde_json::json!({ "ok": true, "id": id, "deleted_at": now })))
}

#[derive(Deserialize)]
pub struct AddKmInput {
    pub km_driven: f64,   // accepts decimals (e.g. 12.4) but stored as integer km
}

/// POST /listings/driver/:id/km
/// Driver phone POSTs the kilometres driven on a completed ride for a specific
/// listing. Backend increments the listing's lifetime odometer.
/// Only the listing's owner can update it (NIP-98 auth required).
pub async fn add_km(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<AddKmInput>,
) -> AppResult<Json<serde_json::Value>> {
    if body.km_driven < 0.0 || body.km_driven > 5000.0 {
        return Err(AppError::BadRequest(
            "km_driven must be between 0 and 5000 (sanity bound)".into()
        ));
    }
    let km_int = body.km_driven.round() as i64;
    let now = chrono::Utc::now().timestamp();

    let result = sqlx::query(
        "UPDATE driver_listings
         SET km_driven_total = km_driven_total + ?1, updated_at = ?2
         WHERE id = ?3 AND driver_pubkey = ?4 AND deleted_at IS NULL"
    )
    .bind(km_int).bind(now).bind(&id).bind(&auth.public_key)
    .execute(&state.db).await?;

    if result.rows_affected() == 0 {
        return Err(AppError::BadRequest(
            "listing not found, not owned by you, or already deleted".into()
        ));
    }

    // Return the new total + service-due hint so the driver UI can update without a refetch.
    let (km_total, service_int): (i64, i64) = sqlx::query_as(
        "SELECT km_driven_total, service_interval_km FROM driver_listings WHERE id = ?1"
    ).bind(&id).fetch_one(&state.db).await?;

    let service_due_km = if service_int > 0 {
        ((km_total / service_int) + 1) * service_int
    } else { 0 };

    Ok(Json(serde_json::json!({
        "ok": true,
        "id": id,
        "km_added": km_int,
        "km_driven_total": km_total,
        "service_interval_km": service_int,
        "service_due_km": service_due_km,
        "km_until_service": (service_due_km - km_total).max(0),
    })))
}

#[derive(Deserialize)]
pub struct NearbyListingsQuery {
    pub lat:           f64,
    pub lng:           f64,
    pub radius_km:     Option<f64>,
    pub category:      Option<String>,
    pub vehicle_type:  Option<String>,
}

#[derive(Serialize, FromRow, Debug)]
pub struct NearbyListingRow {
    pub id:               String,            // listing id
    pub driver_pubkey:    String,
    pub driver_npub:      Option<String>,
    pub listing_name:     Option<String>,
    pub vehicle:          Option<String>,
    pub vehicle_type:     Option<String>,
    pub seats:            Option<i64>,
    pub price_per_km:     Option<i64>,
    pub ride_categories:  Option<String>,
    pub photo_urls:       Option<String>,
    pub description:      Option<String>,
    pub location_country: Option<String>,
    pub location_city:    Option<String>,
    pub lud16:            Option<String>,
    pub lat:              f64,
    pub lng:              f64,
    pub display_name:     Option<String>,    // driver's display name from heartbeat
    pub picture_url:      Option<String>,    // driver's avatar from heartbeat
}

/// GET /rides/nearby-listings
/// Returns one row PER LISTING for online drivers in the search radius.
/// Excludes drivers currently busy with an active ride.
pub async fn nearby_listings(
    State(state): State<AppState>,
    Query(q): Query<NearbyListingsQuery>,
) -> AppResult<Json<Vec<NearbyListingRow>>> {
    let now = chrono::Utc::now().timestamp();
    let radius_deg = q.radius_km.unwrap_or(15.0) / 111.0;
    let category = q.category.as_deref().unwrap_or("city");
    let vtype = q.vehicle_type.as_deref().unwrap_or("");

    // JOIN driver_listings with driver_locations on driver_pubkey.
    // Bookers see one row per LISTING from each online driver.
    let rows: Vec<NearbyListingRow> = sqlx::query_as(
        r#"SELECT dl.id, dl.driver_pubkey, dl.driver_npub, dl.listing_name, dl.vehicle,
                  dl.vehicle_type, dl.seats, dl.price_per_km, dl.ride_categories, dl.photo_urls,
                  dl.description, dl.location_country, dl.location_city, dl.lud16,
                  dloc.lat, dloc.lng, dloc.display_name, dloc.picture_url
           FROM driver_listings dl
           INNER JOIN driver_locations dloc ON dloc.pubkey = dl.driver_pubkey
           WHERE dl.deleted_at IS NULL
             AND dloc.online = 1
             AND dloc.updated_at > ?1
             AND ABS(dloc.lat - ?2) < ?4
             AND ABS(dloc.lng - ?3) < ?4
             AND (?5 = '' OR dl.vehicle_type = ?5)
             AND (dl.ride_categories IS NULL OR dl.ride_categories LIKE '%' || ?6 || '%')
             AND dl.driver_pubkey NOT IN (
               SELECT matched_driver FROM ride_requests
               WHERE matched_driver IS NOT NULL
                 AND status IN ('accepted', 'in_progress', 'funded')
                 AND updated_at > ?7
             )
           ORDER BY ((dloc.lat - ?2)*(dloc.lat - ?2) + (dloc.lng - ?3)*(dloc.lng - ?3)) ASC,
                    dl.updated_at DESC
           LIMIT 50"#
    )
    .bind(now - 120)
    .bind(q.lat).bind(q.lng)
    .bind(radius_deg)
    .bind(vtype)
    .bind(category)
    .bind(now - 7200)
    .fetch_all(&state.db).await?;

    Ok(Json(rows))
}
