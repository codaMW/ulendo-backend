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


    // SECURITY: Atomically claim the release via state transition BEFORE calling Blink.
    // Prevents concurrent release calls from double-spending the merchant payment.
    let claim_now = chrono::Utc::now().timestamp();
    let claim = sqlx::query(
        "UPDATE bookings SET status='releasing', updated_at=?1 WHERE id=?2 AND status IN ('funded','held')"
    )
    .bind(claim_now).bind(&booking_id)
    .execute(&state.db).await?;
    if claim.rows_affected() == 0 {
        return Err(AppError::BadRequest(
            "booking is no longer releasable (another release may be in progress)".into()
        ));
    }

    // Send to merchant (with rollback if Blink fails)
    let blink_result = state.blink
        .send_to_address(&lud16, released_sats, "Ulendo escrow release")
        .await;
    if let Err(e) = blink_result {
        let _ = sqlx::query(
            "UPDATE bookings SET status=?1, updated_at=?2 WHERE id=?3 AND status='releasing'"
        )
        .bind(&booking.status).bind(chrono::Utc::now().timestamp()).bind(&booking_id)
        .execute(&state.db).await;
        return Err(AppError::Payment(format!("payment failed: {e}")));
    }

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

    // SECURITY: Atomically claim the refund via state transition BEFORE calling Blink.
    // Prevents two concurrent refund calls from both reaching the Blink send.
    let now = chrono::Utc::now().timestamp();
    let claim = sqlx::query(
        "UPDATE bookings SET status='refunding', updated_at=?1 WHERE id=?2 AND status IN ('funded','held')"
    )
    .bind(now).bind(&booking_id)
    .execute(&state.db).await?;
    if claim.rows_affected() == 0 {
        return Err(AppError::BadRequest(
            "booking is no longer in a refundable state (another refund may be in progress)".into()
        ));
    }

    // Now call Blink. If it fails, roll status back so retry is possible.
    let blink_result = state.blink
        .send_to_address(lud16, booking.amount_sats, "Ulendo escrow refund")
        .await;

    if let Err(e) = blink_result {
        let _ = sqlx::query(
            "UPDATE bookings SET status=?1, updated_at=?2 WHERE id=?3 AND status='refunding'"
        )
        .bind(&booking.status).bind(chrono::Utc::now().timestamp()).bind(&booking_id)
        .execute(&state.db).await;
        return Err(AppError::Payment(format!("refund failed: {e}")));
    }

    let now2 = chrono::Utc::now().timestamp();
    sqlx::query(
        "UPDATE bookings SET status='refunded', refunded_at=?1, updated_at=?2 WHERE id=?3"
    )
    .bind(now2).bind(now2).bind(&booking_id)
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
    auth: AuthUser,
    Path(booking_id): Path<String>,
    State(state): State<crate::AppState>,
) -> AppResult<Json<EscrowActionResponse>> {
    let booking = fetch_booking(&state, &booking_id).await?;

    // SECURITY: only the merchant (listing owner) can mark complete.
    // Was: NO auth at all — anyone could trigger the 60s auto-release countdown
    // on any booking ID, causing money to flow to the merchant without booker consent.
    let merchant_npub: Option<String> = sqlx::query_scalar(
        "SELECT owner_npub FROM listings WHERE id=?1"
    )
    .bind(&booking.listing_id)
    .fetch_optional(&state.db)
    .await?
    .flatten();
    let merchant_npub = merchant_npub
        .ok_or_else(|| AppError::BadRequest("listing has no owner".into()))?;
    if auth.npub != merchant_npub {
        tracing::warn!("[escrow] complete REJECTED: caller={} is not merchant={} for booking={}",
            &auth.npub[..16.min(auth.npub.len())],
            &merchant_npub[..16.min(merchant_npub.len())],
            &booking_id);
        return Err(AppError::Unauthorized("only the merchant can mark a booking complete".into()));
    }

    if !["funded", "held", "in_progress"].contains(&booking.status.as_str()) {
        return Err(AppError::BadRequest(
            format!("cannot complete from status '{}'", booking.status)
        ));
    }

    // SECURITY: Atomic state transition (defense-in-depth).
    let now = chrono::Utc::now().timestamp();
    let claim = sqlx::query(
        "UPDATE bookings SET status='completed', completed_at=?1, updated_at=?2 WHERE id=?3 AND status IN ('funded','held','in_progress')"
    )
    .bind(now).bind(now).bind(&booking_id)
    .execute(&state.db)
    .await?;
    if claim.rows_affected() == 0 {
        return Err(AppError::BadRequest(
            "booking is no longer in a completable state".into()
        ));
    }

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

// Direct payout from Ulendo escrow wallet — no booking required
pub async fn release_direct(
    auth: crate::auth::AuthUser,
    State(state): State<crate::AppState>,
    Json(body): Json<DirectReleaseRequest>,
) -> AppResult<Json<serde_json::Value>> {
    // ─── SECURITY HARDENED RELEASE_DIRECT ─────────────────────────────────────
    // Previous version trusted request-body fields. New version derives everything
    // from the database and enforces these invariants:
    //   1. Caller is the actual booker of the ride.
    //   2. Ride exists, has a matched driver, and is in a releasable status.
    //   3. Amount comes from ride.fare_sats, NOT the request.
    //   4. lud16 comes from driver's profile, NOT the request.
    //   5. Idempotency via partial UNIQUE INDEX on (ride_id) WHERE status IN ('pending','released').

    // 1. Look up the ride
    let ride: (String, Option<String>, i64, String) = match sqlx::query_as::<_, (String, Option<String>, i64, String)>(
        "SELECT rider_pubkey, matched_driver, fare_sats, status FROM ride_requests WHERE id = ?1"
    )
    .bind(&body.ride_id)
    .fetch_optional(&state.db).await? {
        Some(r) => r,
        None => return Err(AppError::BadRequest("ride not found".into())),
    };
    let (rider_pubkey, matched_driver_opt, fare_sats, status) = ride;

    // 2. AUTH scope: caller must be the booker
    if auth.public_key != rider_pubkey {
        tracing::warn!("[escrow] release_direct REJECTED: caller={} is not booker={} for ride={}",
            &auth.public_key[..8.min(auth.public_key.len())],
            &rider_pubkey[..8.min(rider_pubkey.len())],
            &body.ride_id);
        return Err(AppError::Unauthorized("only the booker can release this ride".into()));
    }

    // 3. State must be releasable
    if status != "accepted" && status != "in_progress" && status != "matched" {
        return Err(AppError::BadRequest(format!(
            "ride not in releasable state (current status: {})", status
        )));
    }

    // 4. Driver must be matched
    let driver_pubkey = matched_driver_opt.ok_or_else(||
        AppError::BadRequest("ride has no matched driver".into())
    )?;

    // 5. lud16 from driver's profile (NOT request body)
    let driver_lud16: String = match sqlx::query_scalar::<_, String>(
        "SELECT lud16 FROM driver_locations WHERE pubkey = ?1"
    )
    .bind(&driver_pubkey)
    .fetch_optional(&state.db).await? {
        Some(s) if !s.is_empty() && s.contains('@') => s,
        _ => return Err(AppError::BadRequest("driver has no valid lightning address on file".into())),
    };

    // 6. Amount from DB (NOT request body)
    let amount_sats = fare_sats;
    if amount_sats <= 0 {
        return Err(AppError::BadRequest("ride fare is zero or invalid".into()));
    }
    let fee_sats = (amount_sats as u64 * state.cfg.escrow_fee_bps / 10_000) as i64;
    let driver_sats = amount_sats - fee_sats;
    if driver_sats < 100 {
        return Err(AppError::BadRequest(format!(
            "amount too low for Lightning (min 100 sats after fee, would pay {} sats)", driver_sats
        )));
    }

    // 7. IDEMPOTENCY: try to insert pending row. The partial UNIQUE INDEX on
    // (ride_id) WHERE status IN ('pending','released') makes this atomic.
    // If insertion fails with a constraint violation, another release is already in flight.
    let release_id = uuid::Uuid::new_v4().to_string().replace('-', "");
    let now = chrono::Utc::now().timestamp();
    let insert_res = sqlx::query(
        "INSERT INTO direct_releases (id, ride_id, payer_pubkey, lud16, amount_sats, driver_sats, fee_sats, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?8)"
    )
    .bind(&release_id).bind(&body.ride_id).bind(&auth.public_key).bind(&driver_lud16)
    .bind(amount_sats).bind(driver_sats).bind(fee_sats).bind(now)
    .execute(&state.db).await;

    if let Err(e) = &insert_res {
        let msg = e.to_string();
        if msg.contains("UNIQUE") || msg.contains("constraint") {
            return Err(AppError::BadRequest("a release for this ride is already pending or completed".into()));
        }
        tracing::error!("[escrow] release_direct insert failed: {}", e);
        return Err(AppError::Internal(anyhow::anyhow!("pending insert failed: {}", e)));
    }

    tracing::info!("[escrow] release_direct authorized: ride={} booker={} driver={} amount={} fee={} → {}",
        &body.ride_id,
        &auth.public_key[..8.min(auth.public_key.len())],
        &driver_pubkey[..8.min(driver_pubkey.len())],
        amount_sats, fee_sats, &driver_lud16);

    // 8. Call Blink
    let blink_result = state.blink.send_to_address(
        &driver_lud16, driver_sats, &format!("Ulendo ride {}", body.ride_id)
    ).await;
    let now2 = chrono::Utc::now().timestamp();

    match blink_result {
        Ok(blink_status) => {
            sqlx::query(
                "UPDATE direct_releases SET status='released', blink_status=?1, updated_at=?2 WHERE id=?3"
            )
            .bind(&blink_status).bind(now2).bind(&release_id)
            .execute(&state.db).await?;
            let _ = sqlx::query(
                "UPDATE ride_requests SET status='completed', updated_at=?1 WHERE id=?2"
            )
            .bind(now2).bind(&body.ride_id)
            .execute(&state.db).await;
            let stats_res = sqlx::query(
                "INSERT INTO driver_stats (pubkey, total_rides, total_earned_sats, updated_at)
                 VALUES (?1, 1, ?2, ?3)
                 ON CONFLICT(pubkey) DO UPDATE SET
                   total_rides       = driver_stats.total_rides + 1,
                   total_earned_sats = driver_stats.total_earned_sats + ?2,
                   updated_at        = ?3"
            ).bind(&driver_pubkey).bind(driver_sats).bind(now2)
            .execute(&state.db).await;
            if let Err(e) = &stats_res {
                tracing::error!("[escrow] driver_stats UPSERT failed: {}", e);
            }
            Ok(Json(serde_json::json!({
                "status": "released",
                "driver_sats": driver_sats,
                "fee_sats": fee_sats,
                "lud16": driver_lud16,
                "release_id": release_id,
                "blink_status": blink_status,
            })))
        }
        Err(e) => {
            let err_msg = format!("Lightning payment failed: {e}");
            tracing::error!("[escrow] Blink failure for ride={}: {}", &body.ride_id, &err_msg);
            sqlx::query(
                "UPDATE direct_releases SET status='failed', error_message=?1, updated_at=?2 WHERE id=?3"
            )
            .bind(&err_msg).bind(now2).bind(&release_id)
            .execute(&state.db).await?;
            Err(AppError::BadRequest(err_msg))
        }
    }
}

// Driver polls this to learn current payout state for a ride.
// Returns the most recent direct_release row for the given ride_id.
pub async fn direct_release_status(
    _auth: crate::auth::AuthUser,
    axum::extract::Path(ride_id): axum::extract::Path<String>,
    State(state): State<crate::AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let row: Option<(String, String, String, i64, i64, i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, status, lud16, amount_sats, driver_sats, fee_sats, error_message, blink_status
         FROM direct_releases WHERE ride_id = ?1
         ORDER BY created_at DESC LIMIT 1"
    )
    .bind(&ride_id)
    .fetch_optional(&state.db).await?;

    match row {
        Some((id, status, lud16, amount_sats, driver_sats, fee_sats, error_message, blink_status)) => {
            Ok(Json(serde_json::json!({
                "release_id": id,
                "ride_id": ride_id,
                "status": status,
                "lud16": lud16,
                "amount_sats": amount_sats,
                "driver_sats": driver_sats,
                "fee_sats": fee_sats,
                "error_message": error_message,
                "blink_status": blink_status,
            })))
        }
        None => Ok(Json(serde_json::json!({
            "ride_id": ride_id,
            "status": "no_release_yet",
        }))),
    }
}

#[derive(serde::Deserialize)]
pub struct DirectReleaseRequest {
    pub lud16: String,
    pub amount_sats: i64,
    pub ride_id: String,
}
