/// Escrow state machine
///
/// pending  → fund    → funded
/// funded   → release → released   (happy path: booker confirms service delivered)
/// funded   → dispute → disputed   (booker raises issue during ride/service)
/// disputed → refund  → refunded   (admin resolves: return funds to booker)
/// any      → (auto)  → cancelled  (invoice expired, or explicit cancel)
///
/// On release: full amount minus fee sent to merchant lud16
/// On dispute: base_fare to merchant, remainder to booker (ride bookings)
///             or full refund to booker (listing bookings)
/// On refund:  full amount to booker lud16_refund

use axum::{extract::{Path, State}, Json};
use serde::{Deserialize, Serialize};
use crate::{
    auth::AuthUser,
    error::{AppError, AppResult},
    routes::bookings::{fetch_booking, notify_booker},
    AppState,
};

// ── Fund ─────────────────────────────────────────────────────────────────────
// Creates a Blink invoice for the booking amount.
// Returns the bolt11 payment_request for the frontend to display as QR.

#[derive(Serialize)]
pub struct FundResponse {
    pub booking_id:      String,
    pub payment_request: String,
    pub payment_hash:    String,
    pub amount_sats:     i64,
    pub expires_at:      i64,
}

pub async fn fund(
    auth: AuthUser,
    Path(booking_id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<FundResponse>> {
    let booking = fetch_booking(&state, &booking_id).await?;

    if booking.booker_npub != auth.npub {
        return Err(AppError::Unauthorized("only the booker can fund this escrow".into()));
    }
    if booking.status != "pending" {
        return Err(AppError::BadRequest(
            format!("booking is '{}', expected 'pending'", booking.status)
        ));
    }

    // Get listing name for invoice memo
    let listing_name: String = sqlx::query_scalar("SELECT name FROM listings WHERE id=?1")
        .bind(&booking.listing_id)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or_else(|| "Ulendo service".into());

    let memo = format!("Ulendo escrow: {} (booking {})", listing_name, &booking_id[..8]);

    let invoice = state.blink
        .create_invoice(booking.amount_sats, &memo)
        .await
        .map_err(|e| AppError::Payment(e.to_string()))?;

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"UPDATE bookings SET
           payment_hash        = ?1,
           payment_request     = ?2,
           invoice_expires_at  = ?3,
           updated_at          = ?4
           WHERE id = ?5"#
    )
    .bind(&invoice.payment_hash)
    .bind(&invoice.payment_request)
    .bind(invoice.expires_at)
    .bind(now)
    .bind(&booking_id)
    .execute(&state.db)
    .await?;

    Ok(Json(FundResponse {
        booking_id,
        payment_request: invoice.payment_request,
        payment_hash:    invoice.payment_hash,
        amount_sats:     booking.amount_sats,
        expires_at:      invoice.expires_at,
    }))
}

// ── Release ───────────────────────────────────────────────────────────────────
// Booker confirms service was delivered → release funds to merchant.

#[derive(Serialize)]
pub struct EscrowActionResponse {
    pub booking_id:    String,
    pub status:        String,
    pub amount_sats:   i64,
    pub fee_sats:      i64,
    pub released_sats: i64,
}

pub async fn release(
    auth: AuthUser,
    Path(booking_id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<EscrowActionResponse>> {
    let booking = fetch_booking(&state, &booking_id).await?;

    if booking.booker_npub != auth.npub {
        return Err(AppError::Unauthorized("only the booker can release escrow".into()));
    }
    if !["funded", "held"].contains(&booking.status.as_str()) {
        return Err(AppError::BadRequest(
            format!("cannot release from status '{}'", booking.status)
        ));
    }

    // Get merchant's lightning address
    let merchant_lud16: Option<String> = sqlx::query_scalar(
        "SELECT l.lud16 FROM listings l WHERE l.id = ?1"
    )
    .bind(&booking.listing_id)
    .fetch_optional(&state.db)
    .await?
    .flatten();

    let lud16 = merchant_lud16
        .ok_or_else(|| AppError::BadRequest("merchant has no lightning address".into()))?;

    let released_sats = booking.amount_sats - booking.fee_sats;

    // Send to merchant
    state.blink
        .send_to_address(&lud16, released_sats, "Ulendo escrow release")
        .await
        .map_err(|e| AppError::Payment(format!("payment failed: {e}")))?;

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE bookings SET status='released', released_at=?1, updated_at=?2 WHERE id=?3"
    )
    .bind(now).bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;

    // Push notification to merchant
    let merchant_npub: Option<String> = sqlx::query_scalar(
        "SELECT owner_npub FROM listings WHERE id=?1"
    )
    .bind(&booking.listing_id)
    .fetch_optional(&state.db)
    .await?
    .flatten();

    if let Some(npub) = merchant_npub {
        let subs = sqlx::query_as::<_, crate::db::PushSubscription>(
            "SELECT * FROM push_subscriptions WHERE npub=?1"
        )
        .bind(&npub)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        let payload = serde_json::json!({
            "title": "Payment released!",
            "body":  format!("{released_sats} sats sent to your Lightning address"),
            "data":  { "booking_id": booking_id, "type": "escrow_released" }
        });
        for sub in subs {
            let _ = state.push.send(&sub, payload.to_string()).await;
        }
    }

    Ok(Json(EscrowActionResponse {
        booking_id,
        status:        "released".into(),
        amount_sats:   booking.amount_sats,
        fee_sats:      booking.fee_sats,
        released_sats,
    }))
}

// ── Dispute ───────────────────────────────────────────────────────────────────
// Booker raises a dispute during a ride or service.
// Ride bookings: merchant gets base_fare, booker gets remainder.
// Listing bookings: full refund to booker.

#[derive(Deserialize)]
pub struct DisputeRequest {
    pub reason: Option<String>,
}

pub async fn dispute(
    auth: AuthUser,
    Path(booking_id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<DisputeRequest>,
) -> AppResult<Json<EscrowActionResponse>> {
    let booking = fetch_booking(&state, &booking_id).await?;

    if booking.booker_npub != auth.npub {
        return Err(AppError::Unauthorized("only the booker can raise a dispute".into()));
    }
    if !["funded", "held", "in_progress"].contains(&booking.status.as_str()) {
        return Err(AppError::BadRequest(
            format!("cannot dispute from status '{}'", booking.status)
        ));
    }

    let now = chrono::Utc::now().timestamp();

    // Dispute split: 30% to driver, 70% refunded to booker
    // For non-ride bookings: full refund
    let (merchant_sats, booker_refund_sats) = if booking.booking_type == "ride" {
        let driver_share = (booking.amount_sats * 30) / 100;
        let booker_share = booking.amount_sats - driver_share;
        (driver_share, booker_share)
    } else {
        (0i64, booking.amount_sats)
    };

    // Refund to booker if they have a refund address
    if booker_refund_sats > 0 {
        if let Some(lud16) = &booking.lud16_refund {
            let _ = state.blink
                .send_to_address(lud16, booker_refund_sats, "Ulendo dispute refund")
                .await
                .map_err(|e| tracing::warn!("refund failed: {e}"));
        }
    }

    // Pay merchant their portion (base fare for rides)
    if merchant_sats > 0 {
        let merchant_lud16: Option<String> = sqlx::query_scalar(
            "SELECT lud16 FROM listings WHERE id=?1"
        )
        .bind(&booking.listing_id)
        .fetch_optional(&state.db)
        .await?
        .flatten();

        if let Some(lud16) = merchant_lud16 {
            let _ = state.blink
                .send_to_address(&lud16, merchant_sats, "Ulendo dispute base fare")
                .await
                .map_err(|e| tracing::warn!("merchant dispute payment failed: {e}"));
        }
    }

    sqlx::query(
        "UPDATE bookings SET status='disputed', disputed_at=?1, updated_at=?2 WHERE id=?3"
    )
    .bind(now).bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;

    tracing::info!(
        booking_id = %booking_id,
        reason = %body.reason.as_deref().unwrap_or("none"),
        "dispute raised"
    );

    Ok(Json(EscrowActionResponse {
        booking_id,
        status:        "disputed".into(),
        amount_sats:   booking.amount_sats,
        fee_sats:      booking.fee_sats,
        released_sats: booker_refund_sats,
    }))
}

// ── Refund ────────────────────────────────────────────────────────────────────
// Full refund — called when invoice expired or booking cancelled before service.

pub async fn refund(
    auth: AuthUser,
    Path(booking_id): Path<String>,
    State(state): State<AppState>,
) -> AppResult<Json<EscrowActionResponse>> {
    let booking = fetch_booking(&state, &booking_id).await?;

    // Booker or listing owner can trigger a refund
    let is_listing_owner: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM listings WHERE id=?1 AND owner_npub=?2)"
    )
    .bind(&booking.listing_id)
    .bind(&auth.npub)
    .fetch_one(&state.db)
    .await?;

    if booking.booker_npub != auth.npub && !is_listing_owner {
        return Err(AppError::Unauthorized("not authorised to refund this booking".into()));
    }

    // Can only refund funded/held bookings
    if !["funded", "held"].contains(&booking.status.as_str()) {
        return Err(AppError::BadRequest(
            format!("cannot refund from status '{}'", booking.status)
        ));
    }

    let lud16 = booking.lud16_refund.as_ref()
        .ok_or_else(|| AppError::BadRequest("no refund address on file".into()))?;

    state.blink
        .send_to_address(lud16, booking.amount_sats, "Ulendo escrow refund")
        .await
        .map_err(|e| AppError::Payment(format!("refund failed: {e}")))?;

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE bookings SET status='refunded', refunded_at=?1, updated_at=?2 WHERE id=?3"
    )
    .bind(now).bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;

    notify_booker(
        &state, &booking,
        "Refund sent",
        &format!("{} sats returned to your Lightning address", booking.amount_sats),
    ).await;

    Ok(Json(EscrowActionResponse {
        booking_id,
        status:        "refunded".into(),
        amount_sats:   booking.amount_sats,
        fee_sats:      0,
        released_sats: booking.amount_sats,
    }))
}
// ── Complete ──────────────────────────────────────────────────────────────────
// Driver marks ride as complete → starts 1-minute auto-release countdown.
// Booker can still release immediately or raise dispute within 60 seconds.

pub async fn complete(
    Path(booking_id): Path<String>,
    State(state): State<crate::AppState>,
) -> AppResult<Json<EscrowActionResponse>> {
    let booking = fetch_booking(&state, &booking_id).await?;

    if !["funded", "held", "in_progress"].contains(&booking.status.as_str()) {
        return Err(AppError::BadRequest(
            format!("cannot complete from status '{}'", booking.status)
        ));
    }

    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE bookings SET status='completed', completed_at=?1, updated_at=?2 WHERE id=?3"
    )
    .bind(now).bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;

    tracing::info!(booking_id = %booking_id, "ride completed — 60s auto-release countdown started");

    // Notify booker: release or dispute within 60 seconds
    notify_booker(
        &state, &booking,
        "Ride complete!",
        "Release payment now or it auto-releases in 60 seconds.",
    ).await;

    // Notify driver via WebSocket
    {
        let reg = state.ws.lock().await;
        // Find the listing owner's pubkey to send WS notification
        let owner_npub: Option<String> = sqlx::query_scalar(
            "SELECT owner_npub FROM listings WHERE id=?1"
        )
        .bind(&booking.listing_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .flatten();

        if let Some(npub) = owner_npub {
            if let Some(tx) = reg.get(&npub) {
                let msg = serde_json::json!({
                    "type": "escrow-completing",
                    "booking_id": booking_id,
                    "auto_release_at": now + 60,
                });
                let _ = tx.send(msg.to_string());
            }
        }
    }

    Ok(Json(EscrowActionResponse {
        booking_id,
        status:        "completed".into(),
        amount_sats:   booking.amount_sats,
        fee_sats:      booking.fee_sats,
        released_sats: 0,
    }))
}
