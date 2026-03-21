use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use crate::{
    auth::AuthUser,
    db::Booking,
    error::{AppError, AppResult},
    AppState,
};

#[derive(Deserialize)]
pub struct CreateBookingRequest {
    pub listing_id:       String,
    pub booking_type:     Option<String>,       // "listing" | "ride"
    pub amount_sats:      i64,
    pub lud16_refund:     Option<String>,       // booker's address for refunds

    // Ride-specific (optional)
    pub ride_id:          Option<String>,
    pub pickup_text:      Option<String>,
    pub destination_text: Option<String>,
    pub pickup_gps_lat:   Option<f64>,
    pub pickup_gps_lng:   Option<f64>,
}

pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<CreateBookingRequest>,
) -> AppResult<Json<Booking>> {
    // Verify the listing exists
    let listing_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM listings WHERE id = ?1 AND available = 1)"
    )
    .bind(&body.listing_id)
    .fetch_one(&state.db)
    .await?;

    if !listing_exists {
        return Err(AppError::NotFound(format!("listing {} not found", body.listing_id)));
    }

    // Validate amount
    if body.amount_sats < 1 {
        return Err(AppError::BadRequest("amount_sats must be positive".into()));
    }

    // Calculate platform fee
    let fee_sats = (body.amount_sats as u64 * state.cfg.escrow_fee_bps / 10_000) as i64;

    let booking_type = body.booking_type.as_deref().unwrap_or("listing");
    if !["listing", "ride"].contains(&booking_type) {
        return Err(AppError::BadRequest("booking_type must be listing or ride".into()));
    }

    let id  = uuid::Uuid::new_v4().to_string().replace('-', "");
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        r#"INSERT INTO bookings
           (id, listing_id, booker_npub, booking_type, status, amount_sats, fee_sats,
            lud16_refund, ride_id, pickup_text, destination_text,
            pickup_gps_lat, pickup_gps_lng, created_at, updated_at)
           VALUES (?1,?2,?3,?4,'pending',?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"#
    )
    .bind(&id)
    .bind(&body.listing_id)
    .bind(&auth.npub)
    .bind(booking_type)
    .bind(body.amount_sats)
    .bind(fee_sats)
    .bind(&body.lud16_refund)
    .bind(&body.ride_id)
    .bind(&body.pickup_text)
    .bind(&body.destination_text)
    .bind(body.pickup_gps_lat)
    .bind(body.pickup_gps_lng)
    .bind(now)
    .bind(now)
    .execute(&state.db)
    .await?;

    let booking = fetch_booking(&state, &id).await?;
    Ok(Json(booking))
}

pub async fn get_one(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<Booking>> {
    let booking = fetch_booking(&state, &id).await?;

    // Only booker or listing owner can view
    let is_owner: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM listings WHERE id=?1 AND owner_npub=?2)"
    )
    .bind(&booking.listing_id)
    .bind(&auth.npub)
    .fetch_one(&state.db)
    .await?;

    if booking.booker_npub != auth.npub && !is_owner {
        return Err(AppError::Unauthorized("access denied".into()));
    }

    Ok(Json(booking))
}

#[derive(Deserialize)]
pub struct UpdateStatusRequest {
    pub status: String,
}

/// Merchant can mark a booking as 'held' once they've confirmed receipt of escrow.
/// Other transitions (funded, released, disputed) go through /escrow routes.
pub async fn update_status(
    auth: AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<UpdateStatusRequest>,
) -> AppResult<Json<Booking>> {
    let booking = fetch_booking(&state, &id).await?;

    // Only 'held' transition is allowed here (merchant acknowledges funds)
    if body.status != "held" {
        return Err(AppError::BadRequest(
            "use /escrow routes for release, dispute, and refund".into()
        ));
    }

    // Must be the listing owner
    let is_listing_owner: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM listings WHERE id=?1 AND owner_npub=?2)"
    )
    .bind(&booking.listing_id)
    .bind(&auth.npub)
    .fetch_one(&state.db)
    .await?;

    if !is_listing_owner {
        return Err(AppError::Unauthorized("only the merchant can confirm receipt".into()));
    }

    if booking.status != "funded" {
        return Err(AppError::BadRequest(
            format!("can't transition from '{}' to 'held'", booking.status)
        ));
    }

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE bookings SET status='held', held_at=?1, updated_at=?2 WHERE id=?3"
    )
    .bind(now).bind(now).bind(&id)
    .execute(&state.db)
    .await?;

    // Notify booker their funds are held
    notify_booker(&state, &booking, "Merchant confirmed", "Your funds are held in escrow.").await;

    let updated = fetch_booking(&state, &id).await?;
    Ok(Json(updated))
}

// ── helpers ───────────────────────────────────────────────────────────────────

pub async fn fetch_booking(state: &AppState, id: &str) -> AppResult<Booking> {
    sqlx::query_as::<_, Booking>("SELECT * FROM bookings WHERE id = ?1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("booking {id} not found")))
}

pub async fn notify_booker(state: &AppState, booking: &Booking, title: &str, body: &str) {
    let subs = sqlx::query_as::<_, crate::db::PushSubscription>(
        "SELECT * FROM push_subscriptions WHERE npub = ?1"
    )
    .bind(&booking.booker_npub)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let payload = serde_json::json!({
        "title": title,
        "body":  body,
        "data":  { "booking_id": booking.id, "type": "booking_update" }
    });

    for sub in subs {
        let _ = state.push.send(&sub, payload.to_string()).await;
    }
}