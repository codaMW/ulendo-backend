/// Dual confirmation routes — rider + driver confirm trip completion
///
/// Flow:
///   1. Driver taps "Mark complete" → POST /escrow/:id/driver-confirm
///   2. Rider taps "Confirm arrival" → POST /escrow/:id/rider-confirm
///   3. If BOTH confirmed → instant release (90% driver, 10% Ulendo)
///   4. If only ONE confirmed → auto-release after 30 min
///   5. If NEITHER after in_progress → auto-release after 2 hours
///
/// Also:
///   - POST /escrow/:id/pickup → driver confirms pickup (starts 15m no-show refund timer)
///   - POST /escrow/:id/cancel → cancel before pickup → full refund

use axum::{extract::{Path, State}, Json};
use serde::Serialize;
use crate::{
    auth::AuthUser,
    error::{AppError, AppResult},
    routes::bookings::fetch_booking,
    AppState,
};

#[derive(Serialize)]
pub struct ConfirmResponse {
    pub booking_id: String,
    pub status: String,
    pub rider_confirmed: bool,
    pub driver_confirmed: bool,
    pub released: bool,
    pub released_sats: i64,
    pub fee_sats: i64,
}

/// Driver confirms pickup happened
pub async fn confirm_pickup(
    auth: AuthUser,
    Path(booking_id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<ConfirmResponse>> {
    let booking = fetch_booking(&state, &booking_id).await?;

    // Only the listing owner (driver) can confirm pickup
    let owner_npub: Option<String> = sqlx::query_scalar(
        "SELECT owner_npub FROM listings WHERE id=?1"
    )
    .bind(&booking.listing_id)
    .fetch_optional(&state.db)
    .await?
    .flatten();

    if owner_npub.as_deref() != Some(&auth.npub) {
        return Err(AppError::Unauthorized("only the driver can confirm pickup".into()));
    }

    if !["funded", "held"].contains(&booking.status.as_str()) {
        return Err(AppError::BadRequest(
            format!("cannot confirm pickup from status '{}'", booking.status)
        ));
    }

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE bookings SET status='in_progress', pickup_confirmed_at=?1, updated_at=?2 WHERE id=?3"
    )
    .bind(now).bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;

    tracing::info!(booking_id = %booking_id, "pickup confirmed by driver");

    Ok(Json(ConfirmResponse {
        booking_id,
        status: "in_progress".into(),
        rider_confirmed: false,
        driver_confirmed: false,
        released: false,
        released_sats: 0,
        fee_sats: 0,
    }))
}

/// Driver confirms trip completion
pub async fn driver_confirm(
    auth: AuthUser,
    Path(booking_id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<ConfirmResponse>> {
    let booking = fetch_booking(&state, &booking_id).await?;

    let owner_npub: Option<String> = sqlx::query_scalar(
        "SELECT owner_npub FROM listings WHERE id=?1"
    )
    .bind(&booking.listing_id)
    .fetch_optional(&state.db)
    .await?
    .flatten();

    if owner_npub.as_deref() != Some(&auth.npub) {
        return Err(AppError::Unauthorized("only the driver can confirm completion".into()));
    }

    if !["funded", "held", "in_progress"].contains(&booking.status.as_str()) {
        return Err(AppError::BadRequest(
            format!("cannot confirm from status '{}'", booking.status)
        ));
    }

    let now = chrono::Utc::now().timestamp();
    let rider_already = booking.rider_confirmed_at.is_some();

    sqlx::query(
        "UPDATE bookings SET driver_confirmed_at=?1, updated_at=?2 WHERE id=?3"
    )
    .bind(now).bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;

    // If rider already confirmed → instant release
    if rider_already {
        return do_instant_release(&state, &booking_id, booking.amount_sats).await;
    }

    // Otherwise mark completed, wait for rider or 30m timeout
    sqlx::query(
        "UPDATE bookings SET status='completed', completed_at=?1 WHERE id=?2 AND status != 'released'"
    )
    .bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;

    tracing::info!(booking_id = %booking_id, "driver confirmed — waiting for rider (30m timeout)");

    Ok(Json(ConfirmResponse {
        booking_id,
        status: "completed".into(),
        rider_confirmed: false,
        driver_confirmed: true,
        released: false,
        released_sats: 0,
        fee_sats: 0,
    }))
}

/// Rider confirms trip completion
pub async fn rider_confirm(
    auth: AuthUser,
    Path(booking_id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<ConfirmResponse>> {
    let booking = fetch_booking(&state, &booking_id).await?;

    if booking.booker_npub != auth.npub {
        return Err(AppError::Unauthorized("only the rider can confirm".into()));
    }

    if !["funded", "held", "in_progress", "completed"].contains(&booking.status.as_str()) {
        return Err(AppError::BadRequest(
            format!("cannot confirm from status '{}'", booking.status)
        ));
    }

    let now = chrono::Utc::now().timestamp();
    let driver_already = booking.driver_confirmed_at.is_some();

    sqlx::query(
        "UPDATE bookings SET rider_confirmed_at=?1, updated_at=?2 WHERE id=?3"
    )
    .bind(now).bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;

    // If driver already confirmed → instant release
    if driver_already {
        return do_instant_release(&state, &booking_id, booking.amount_sats).await;
    }

    // Otherwise mark completed, wait for driver or 30m timeout
    sqlx::query(
        "UPDATE bookings SET status='completed', completed_at=?1 WHERE id=?2 AND status != 'released'"
    )
    .bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;

    tracing::info!(booking_id = %booking_id, "rider confirmed — waiting for driver (30m timeout)");

    Ok(Json(ConfirmResponse {
        booking_id,
        status: "completed".into(),
        rider_confirmed: true,
        driver_confirmed: false,
        released: false,
        released_sats: 0,
        fee_sats: 0,
    }))
}

/// Cancel before pickup → full refund
pub async fn cancel_before_pickup(
    auth: AuthUser,
    Path(booking_id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<ConfirmResponse>> {
    let booking = fetch_booking(&state, &booking_id).await?;

    if booking.booker_npub != auth.npub {
        return Err(AppError::Unauthorized("only the rider can cancel".into()));
    }

    // Can only cancel before pickup
    if booking.pickup_confirmed_at.is_some() {
        return Err(AppError::BadRequest("cannot cancel after pickup — trip is in progress".into()));
    }

    if !["pending", "funded", "held"].contains(&booking.status.as_str()) {
        return Err(AppError::BadRequest(
            format!("cannot cancel from status '{}'", booking.status)
        ));
    }

    let now = chrono::Utc::now().timestamp();

    // Refund if funded
    if ["funded", "held"].contains(&booking.status.as_str()) {
        if let Some(lud16) = &booking.lud16_refund {
            let _ = state.blink
                .send_to_address(lud16, booking.amount_sats, "Ulendo cancellation refund")
                .await
                .map_err(|e| tracing::warn!("cancel refund failed: {e}"));
        }
    }

    sqlx::query(
        "UPDATE bookings SET status='cancelled', cancelled_at=?1, updated_at=?2 WHERE id=?3"
    )
    .bind(now).bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;

    tracing::info!(booking_id = %booking_id, "booking cancelled before pickup — refunded");

    Ok(Json(ConfirmResponse {
        booking_id,
        status: "cancelled".into(),
        rider_confirmed: false,
        driver_confirmed: false,
        released: false,
        released_sats: booking.amount_sats,
        fee_sats: 0,
    }))
}

/// Internal: both parties confirmed → release immediately
async fn do_instant_release(
    state: &AppState,
    booking_id: &str,
    amount_sats: i64,
) -> AppResult<Json<ConfirmResponse>> {
    let fee_bps = state.cfg.escrow_fee_bps as i64;
    let fee_sats = (amount_sats * fee_bps) / 10_000;
    let driver_sats = amount_sats - fee_sats;

    // Get driver's lightning address
    let driver_lud16: Option<String> = sqlx::query_scalar(
        "SELECT l.lud16 FROM listings l JOIN bookings b ON b.listing_id = l.id WHERE b.id = ?1"
    )
    .bind(booking_id)
    .fetch_optional(&state.db)
    .await?
    .flatten();

    let lud16 = driver_lud16
        .ok_or_else(|| AppError::BadRequest("driver has no lightning address".into()))?;

    // Pay the driver
    state.blink
        .send_to_address(&lud16, driver_sats, &format!("Ulendo ride payment ({})", &booking_id[..8]))
        .await
        .map_err(|e| AppError::Payment(format!("payment failed: {e}")))?;

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE bookings SET status='released', fee_sats=?1, released_at=?2, completed_at=COALESCE(completed_at,?2), updated_at=?3 WHERE id=?4"
    )
    .bind(fee_sats).bind(now).bind(now).bind(booking_id)
    .execute(&state.db)
    .await?;

    tracing::info!(
        booking_id = %booking_id,
        driver_sats = driver_sats,
        fee_sats = fee_sats,
        "INSTANT RELEASE — both parties confirmed"
    );

    Ok(Json(ConfirmResponse {
        booking_id: booking_id.to_string(),
        status: "released".into(),
        rider_confirmed: true,
        driver_confirmed: true,
        released: true,
        released_sats: driver_sats,
        fee_sats,
    }))
}
