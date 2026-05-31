use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use crate::{AppState, auth::AuthUser, error::{AppError, AppResult}};

// ─── GPS Heartbeat (called from ws.rs) ────────────────────────────────────
#[derive(Deserialize, Debug)]
pub struct GpsHeartbeat {
    pub lat: f64,
    pub lng: f64,
    pub heading: Option<f64>,
    pub speed_kmh: Option<f64>,
    pub vehicle_type: Option<String>,
    pub ride_categories: Option<String>,
    pub seats: Option<i64>,
    pub lud16: Option<String>,
    pub display_name: Option<String>,
    pub picture_url: Option<String>,
    pub country: Option<String>,
    pub city: Option<String>,
    pub price_per_km: Option<i64>,
}

pub async fn upsert_driver_location(state: &AppState, pubkey: &str, hb: &GpsHeartbeat) {
    let now = chrono::Utc::now().timestamp();
    let _ = sqlx::query(
        r#"INSERT INTO driver_locations
           (pubkey, lat, lng, heading, speed_kmh, vehicle_type, ride_categories,
            seats, lud16, display_name, picture_url, country, city, online, updated_at, price_per_km)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,1,?14,?15)
           ON CONFLICT(pubkey) DO UPDATE SET
            lat=?2, lng=?3, heading=?4, speed_kmh=?5,
            vehicle_type=COALESCE(?6, driver_locations.vehicle_type),
            ride_categories=COALESCE(?7, driver_locations.ride_categories),
            seats=COALESCE(?8, driver_locations.seats),
            lud16=COALESCE(?9, driver_locations.lud16),
            display_name=COALESCE(?10, driver_locations.display_name),
            picture_url=COALESCE(?11, driver_locations.picture_url),
            country=COALESCE(?12, driver_locations.country),
            city=COALESCE(?13, driver_locations.city),
            online=1, updated_at=?14,
            price_per_km=COALESCE(?15, driver_locations.price_per_km)"#
    )
    .bind(pubkey).bind(hb.lat).bind(hb.lng)
    .bind(hb.heading).bind(hb.speed_kmh)
    .bind(&hb.vehicle_type).bind(&hb.ride_categories)
    .bind(hb.seats).bind(&hb.lud16)
    .bind(&hb.display_name).bind(&hb.picture_url)
    .bind(&hb.country).bind(&hb.city)
    .bind(now).bind(hb.price_per_km)
    .execute(&state.db)
    .await;
}

pub async fn mark_driver_offline(state: &AppState, pubkey: &str) {
    let _ = sqlx::query("UPDATE driver_locations SET online=0 WHERE pubkey=?1")
        .bind(pubkey)
        .execute(&state.db)
        .await;
}

// ─── Ride Request: rider submits, backend finds nearest drivers ───────────
#[derive(Deserialize)]
pub struct RideRequestInput {
    pub pickup_lat: f64,
    pub pickup_lng: f64,
    pub dest_lat: Option<f64>,
    pub dest_lng: Option<f64>,
    pub pickup_text: Option<String>,
    pub dest_text: Option<String>,
    pub vehicle_pref: Option<String>,
    pub ride_category: Option<String>,
    pub estimated_km: Option<f64>,
    pub fare_sats: i64,
}

#[derive(Serialize)]
pub struct RideRequestResponse {
    pub ride_id: String,
    pub status: String,
    pub drivers_notified: i32,
}

pub async fn request_ride(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<RideRequestInput>,
) -> AppResult<Json<RideRequestResponse>> {
    // PHASE 2 FIX: enforce minimum fare so we don't accept bookings that would later
    // fail at Lightning payout (driver_sats < 100 after fee).
    let min_fare = state.cfg.min_fare_sats;
    if body.fare_sats < min_fare {
        return Err(AppError::BadRequest(format!(
            "Minimum fare is {} sats (you offered {}). Increase your trip estimate or choose a more economical option.",
            min_fare, body.fare_sats
        )));
    }
    let ride_id = uuid::Uuid::new_v4().to_string().replace('-', "");
    let now = chrono::Utc::now().timestamp();
    let category = body.ride_category.as_deref().unwrap_or("city");
    let vehicle_pref = body.vehicle_pref.as_deref().unwrap_or("");

    // Find nearest online drivers within radius (haversine approximation)
    // 1 degree ≈ 111km. Start with ~15km radius for city, ~100km for intercity
    let radius_deg = match category {
        "intercity" => 1.0,
        "tourist" => 2.0,
        _ => 0.15, // ~15km for city
    };

    let drivers: Vec<(String,)> = sqlx::query_as(
        r#"SELECT pubkey FROM driver_locations
           WHERE online = 1
             AND updated_at > ?1
             AND ABS(lat - ?2) < ?4
             AND ABS(lng - ?3) < ?4
             AND (?5 = '' OR vehicle_type = ?5)
             AND ride_categories LIKE '%' || ?6 || '%'
             AND pubkey != ?7
           ORDER BY ((lat - ?2)*(lat - ?2) + (lng - ?3)*(lng - ?3)) ASC
           LIMIT 5"#
    )
    .bind(now - 120) // online within last 2 minutes
    .bind(body.pickup_lat)
    .bind(body.pickup_lng)
    .bind(radius_deg)
    .bind(vehicle_pref)
    .bind(category)
    .bind(&auth.public_key)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let driver_pubkeys: Vec<String> = drivers.into_iter().map(|d| d.0).collect();
    let notified_count = driver_pubkeys.len() as i32;
    let drivers_json = serde_json::to_string(&driver_pubkeys).unwrap_or_default();

    // Insert ride request
    sqlx::query(
        r#"INSERT INTO ride_requests
           (id, rider_pubkey, rider_npub, pickup_lat, pickup_lng, dest_lat, dest_lng,
            pickup_text, dest_text, vehicle_pref, ride_category, estimated_km,
            fare_sats, status, drivers_notified, accept_deadline, round, created_at, updated_at)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'searching',?14,?15,1,?16,?16)"#
    )
    .bind(&ride_id)
    .bind(&auth.public_key)
    .bind(&auth.npub)
    .bind(body.pickup_lat).bind(body.pickup_lng)
    .bind(body.dest_lat).bind(body.dest_lng)
    .bind(&body.pickup_text).bind(&body.dest_text)
    .bind(vehicle_pref).bind(category)
    .bind(body.estimated_km)
    .bind(body.fare_sats)
    .bind(&drivers_json)
    .bind(now + 60) // 60 second accept window
    .bind(now)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    // Notify each driver via WebSocket
    for dpk in &driver_pubkeys {
        let payload = serde_json::json!({
            "type": "ulendo-ride-offer",
            "rideId": ride_id,
            "pickupLat": body.pickup_lat,
            "pickupLng": body.pickup_lng,
            "destLat": body.dest_lat,
            "destLng": body.dest_lng,
            "pickupText": body.pickup_text,
            "destText": body.dest_text,
            "fareSats": body.fare_sats,
            "estimatedKm": body.estimated_km,
            "category": category,
            "riderPubkey": auth.public_key,
        });
        let msg = serde_json::json!({
            "to": dpk,
            "from": "server",
            "type": "ulendo-ride-offer",
            "payload": payload,
        });
        let reg = state.ws.lock().await;
        if let Some(tx) = reg.get(dpk) {
            let _ = tx.send(msg.to_string());
        }
        // Also send push notification
        drop(reg);
        let subs = sqlx::query_as::<_, crate::db::PushSubscription>(
            "SELECT ps.* FROM push_subscriptions ps JOIN identities i ON i.npub = ps.npub WHERE i.public_key = ?1"
        ).bind(dpk).fetch_all(&state.db).await.unwrap_or_default();
        let push_payload = serde_json::json!({
            "title": "🚗 New ride request!",
            "body": format!("{} sats · {}", body.fare_sats, body.pickup_text.as_deref().unwrap_or("Nearby")),
            "data": { "type": "ride_offer", "ride_id": ride_id },
        });
        for sub in &subs {
            let _ = state.push.send(sub, push_payload.to_string()).await;
        }
    }

    tracing::info!("[rides] request {} — notified {} drivers", &ride_id[..8], notified_count);

    Ok(Json(RideRequestResponse {
        ride_id,
        status: if notified_count > 0 { "searching".into() } else { "no_drivers".into() },
        drivers_notified: notified_count,
    }))
}

// ─── Driver Accept: first driver to accept wins ───────────────────────────
#[derive(Deserialize)]
pub struct AcceptRideInput {
    pub ride_id: String,
}

#[derive(Serialize)]
pub struct AcceptRideResponse {
    pub status: String,
    pub ride_id: String,
    pub rider_pubkey: String,
    pub pickup_lat: f64,
    pub pickup_lng: f64,
    pub dest_lat: Option<f64>,
    pub dest_lng: Option<f64>,
    pub fare_sats: i64,
}

pub async fn accept_ride(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<AcceptRideInput>,
) -> AppResult<Json<AcceptRideResponse>> {
    let now = chrono::Utc::now().timestamp();

    // SECURITY: Driver eligibility — must have a profile with valid lud16
    let driver_lud16: Option<String> = sqlx::query_scalar(
        "SELECT lud16 FROM driver_locations WHERE pubkey=?1"
    ).bind(&auth.public_key).fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;
    let driver_lud16 = driver_lud16.unwrap_or_default();
    if driver_lud16.is_empty() || !driver_lud16.contains('@') {
        return Err(AppError::BadRequest(
            "driver has no valid lightning address — set one in profile before accepting rides".into()
        ));
    }

    // SECURITY: Self-booking guard — fetch rider_pubkey first to make sure caller != rider
    let rider_pubkey: Option<String> = sqlx::query_scalar(
        "SELECT rider_pubkey FROM ride_requests WHERE id=?1"
    ).bind(&body.ride_id).fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;
    let rider_pubkey = rider_pubkey.ok_or_else(||
        AppError::BadRequest("ride not found".into())
    )?;
    if auth.public_key == rider_pubkey {
        tracing::warn!("[rides] accept REJECTED: caller={} tried to accept own ride={}",
            &auth.public_key[..8.min(auth.public_key.len())], &body.ride_id);
        return Err(AppError::BadRequest(
            "cannot accept your own ride request".into()
        ));
    }

    // Atomic: only the first driver to accept gets the ride
    let rows = sqlx::query(
        "UPDATE ride_requests SET status='accepted', matched_driver=?1, updated_at=?2
         WHERE id=?3 AND status='searching'"
    )
    .bind(&auth.public_key)
    .bind(now)
    .bind(&body.ride_id)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    if rows.rows_affected() == 0 {
        return Err(AppError::BadRequest("Ride already taken or expired".into()));
    }

    // Fetch ride details
    // Fetch ride details + estimated_km (for server-side fare recomputation)
    let ride: (String, f64, f64, Option<f64>, Option<f64>, i64, String, Option<f64>) = sqlx::query_as(
        "SELECT rider_pubkey, pickup_lat, pickup_lng, dest_lat, dest_lng, fare_sats, drivers_notified, estimated_km
         FROM ride_requests WHERE id=?1"
    )
    .bind(&body.ride_id)
    .fetch_one(&state.db)
    .await
    .map_err(|_| AppError::NotFound("ride not found".into()))?;
    let (rider_pk, plat, plng, dlat, dlng, requested_fare, notified_json, estimated_km_opt) = ride;

    // SECURITY: Recompute fare from THIS DRIVER's current price_per_km × estimated_km.
    // Previously trusted body.fare_sats (calculated by the rider client from a possibly
    // stale cached driver price). Now: driver_listings price is authoritative.
    let driver_ppk: Option<i64> = sqlx::query_scalar(
        "SELECT price_per_km FROM driver_listings WHERE driver_pubkey=?1 AND deleted_at IS NULL ORDER BY updated_at DESC LIMIT 1"
    ).bind(&auth.public_key).fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;
    let mut driver_ppk = driver_ppk.unwrap_or(0);
    if driver_ppk <= 0 {
        driver_ppk = sqlx::query_scalar::<_, i64>(
            "SELECT price_per_km FROM driver_locations WHERE pubkey=?1"
        ).bind(&auth.public_key).fetch_optional(&state.db).await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?
        .unwrap_or(0);
    }
    let estimated_km = estimated_km_opt.unwrap_or(0.0);
    let recomputed_fare: i64 = if driver_ppk > 0 && estimated_km > 0.0 {
        (driver_ppk as f64 * estimated_km).round() as i64
    } else {
        requested_fare
    };
    if recomputed_fare != requested_fare {
        tracing::info!("[rides] fare recomputed for accept: ride={} driver={} requested={} -> actual={} (ppk={} km={})",
            &body.ride_id,
            &auth.public_key[..8.min(auth.public_key.len())],
            requested_fare, recomputed_fare, driver_ppk, estimated_km);
        let _ = sqlx::query(
            "UPDATE ride_requests SET fare_sats=?1, updated_at=?2 WHERE id=?3"
        ).bind(recomputed_fare).bind(now).bind(&body.ride_id)
        .execute(&state.db).await;
    }
    let fare = recomputed_fare;

    // Notify rider: "Driver found!"
    let accept_msg = serde_json::json!({
        "to": rider_pk,
        "from": "server",
        "type": "ulendo-ride-accepted",
        "payload": {
            "rideId": body.ride_id,
            "driverPubkey": auth.public_key,
            "fareSats": fare,
        },
    });
    {
        let reg = state.ws.lock().await;
        if let Some(tx) = reg.get(&rider_pk) {
            let _ = tx.send(accept_msg.to_string());
        }
    }

    // Notify other drivers: "Ride taken"
    let other_drivers: Vec<String> = serde_json::from_str(&notified_json).unwrap_or_default();
    for dpk in &other_drivers {
        if dpk == &auth.public_key { continue; }
        let taken_msg = serde_json::json!({
            "to": dpk,
            "from": "server",
            "type": "ulendo-ride-taken",
            "payload": { "rideId": body.ride_id },
        });
        let reg = state.ws.lock().await;
        if let Some(tx) = reg.get(dpk) {
            let _ = tx.send(taken_msg.to_string());
        }
    }

    tracing::info!("[rides] {} accepted by {}", &body.ride_id[..8], &auth.public_key[..8]);

    Ok(Json(AcceptRideResponse {
        status: "accepted".into(),
        ride_id: body.ride_id,
        rider_pubkey: rider_pk,
        pickup_lat: plat,
        pickup_lng: plng,
        dest_lat: dlat,
        dest_lng: dlng,
        fare_sats: fare,
    }))
}

// ─── Nearby Drivers: for the rider's map ──────────────────────────────────
#[derive(Deserialize)]
pub struct NearbyQuery {
    pub lat: f64,
    pub lng: f64,
    pub radius_km: Option<f64>,
    pub category: Option<String>,
    pub vehicle_type: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct NearbyDriver {
    pub pubkey: String,
    pub lat: f64,
    pub lng: f64,
    pub vehicle_type: String,
    pub ride_categories: String,
    pub seats: i64,
    pub display_name: String,
    pub picture_url: String,
    pub lud16: String,
    pub price_per_km: i64,
}

pub async fn nearby_drivers(
    State(state): State<AppState>,
    Json(body): Json<NearbyQuery>,
) -> AppResult<Json<Vec<NearbyDriver>>> {
    let now = chrono::Utc::now().timestamp();
    let radius_deg = body.radius_km.unwrap_or(15.0) / 111.0;
    let category = body.category.as_deref().unwrap_or("city");
    let vtype = body.vehicle_type.as_deref().unwrap_or("");

    // Drivers with an active ride (accepted/in_progress/funded) are NOT available.
    // Excluded via NOT IN subquery against ride_requests.
    let drivers = sqlx::query_as::<_, NearbyDriver>(
        r#"SELECT dl.pubkey, dl.lat, dl.lng, dl.vehicle_type, dl.ride_categories, dl.seats,
                  dl.display_name, dl.picture_url, dl.lud16, dl.price_per_km
           FROM driver_locations dl
           WHERE dl.online = 1
             AND dl.updated_at > ?1
             AND ABS(dl.lat - ?2) < ?4
             AND ABS(dl.lng - ?3) < ?4
             AND (?5 = '' OR dl.vehicle_type = ?5)
             AND dl.ride_categories LIKE '%' || ?6 || '%'
             AND dl.pubkey NOT IN (
               SELECT matched_driver FROM ride_requests
               WHERE matched_driver IS NOT NULL
                 AND status IN ('accepted', 'in_progress', 'funded')
                 AND updated_at > ?7
             )
           ORDER BY ((dl.lat - ?2)*(dl.lat - ?2) + (dl.lng - ?3)*(dl.lng - ?3)) ASC
           LIMIT 20"#
    )
    .bind(now - 120)
    .bind(body.lat).bind(body.lng)
    .bind(radius_deg)
    .bind(vtype)
    .bind(category)
    .bind(now - 7200)  // ?7: ignore stale active rides older than 2 hours
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    Ok(Json(drivers))
}

// ─── Debug: manually register a test driver location ───────────────────────
#[derive(Deserialize)]
pub struct TestDriverInput {
    pub pubkey: String,
    pub lat: f64,
    pub lng: f64,
    pub name: Option<String>,
    pub vehicle_type: Option<String>,
}

pub async fn test_add_driver(
    State(state): State<AppState>,
    Json(body): Json<TestDriverInput>,
) -> AppResult<Json<serde_json::Value>> {
    let hb = GpsHeartbeat {
        lat: body.lat, lng: body.lng,
        heading: None, speed_kmh: None,
        vehicle_type: body.vehicle_type.or(Some("sedan".into())),
        ride_categories: Some("city".into()),
        seats: Some(4), lud16: Some("".into()),
        display_name: body.name.or(Some("Test Driver".into())),
        picture_url: None, country: None, city: None,
        price_per_km: Some(500),
    };
    upsert_driver_location(&state, &body.pubkey, &hb).await;
    Ok(Json(serde_json::json!({"ok": true, "pubkey": body.pubkey, "lat": body.lat, "lng": body.lng})))
}


// ─── HTTP Heartbeat (REST fallback for mobile) ────────────────────────────
pub async fn http_heartbeat(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(hb): Json<GpsHeartbeat>,
) -> AppResult<Json<serde_json::Value>> {
    upsert_driver_location(&state, &auth.public_key, &hb).await;
    Ok(Json(serde_json::json!({"ok": true})))
}

// ─── HTTP booking notification (fallback when WS fails) ───────────────────
#[derive(Deserialize)]
pub struct HttpBookingNotify {
    pub driver_pubkey: String,
    pub ride_id: String,
    pub pickup: Option<String>,
    pub destination: Option<String>,
    pub fare_sats: Option<i64>,
    pub passenger_pubkey: Option<String>,
    pub passenger_npub: Option<String>,
}

pub async fn http_send_booking(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<HttpBookingNotify>,
) -> AppResult<Json<serde_json::Value>> {
    // Store booking in ride_requests so the matched driver can poll for it
    let now = chrono::Utc::now().timestamp();
    let pickup_text = body.pickup.clone().unwrap_or_default();
    let dest_text = body.destination.clone().unwrap_or_default();
    let fare = body.fare_sats.unwrap_or(0);
    let insert_result = sqlx::query(
        "INSERT INTO ride_requests
         (id, rider_pubkey, rider_npub, pickup_lat, pickup_lng, pickup_text, dest_text,
          fare_sats, status, matched_driver, created_at, updated_at)
         VALUES (?1, ?2, ?3, 0, 0, ?4, ?5, ?6, 'pending', ?7, ?8, ?8)
         ON CONFLICT(id) DO UPDATE SET
           matched_driver = ?7, status = 'pending', updated_at = ?8"
    )
    .bind(&body.ride_id)
    .bind(&auth.public_key)
    .bind(body.passenger_npub.as_deref().unwrap_or(""))
    .bind(&pickup_text)
    .bind(&dest_text)
    .bind(fare)
    .bind(&body.driver_pubkey)
    .bind(now)
    .execute(&state.db).await;

    match &insert_result {
        Ok(_) => tracing::info!("[booking] stored ride {} for driver {}", &body.ride_id, &body.driver_pubkey[..8]),
        Err(e) => tracing::error!("[booking] DB insert failed: {}", e),
    }

    let msg = serde_json::json!({
        "to": body.driver_pubkey,
        "from": auth.public_key,
        "type": "ulendo-booking-request",
        "payload": {
            "rideId": body.ride_id,
            "pickup": body.pickup.unwrap_or_default(),
            "destination": body.destination.unwrap_or_default(),
            "fareSats": body.fare_sats.unwrap_or(0),
            "passengerPubkey": body.passenger_pubkey.unwrap_or_default(),
            "passengerNpub": body.passenger_npub.unwrap_or_default(),
        }
    });
    // Try WS delivery to the matched driver only
    let mut delivered = false;
    {
        let reg = state.ws.lock().await;
        if let Some(tx) = reg.get(&body.driver_pubkey) {
            let _ = tx.send(msg.to_string());
            delivered = true;
            tracing::info!("[booking] WS delivered to driver {}", &body.driver_pubkey[..8]);
        } else {
            tracing::info!("[booking] driver {} not on WS, will rely on HTTP poll", &body.driver_pubkey[..8]);
        }
    }

    Ok(Json(serde_json::json!({"ok": true, "ws_delivered": delivered})))
}

// ─── Driver polls for pending bookings ─────────────────────────────────────
pub async fn poll_bookings(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<serde_json::Value>>> {
    let cutoff = chrono::Utc::now().timestamp() - 300;
    let rows = sqlx::query_as::<_, (String, String, i64, i64, String, String)>(
        "SELECT id, rider_pubkey, fare_sats, created_at, pickup_text, dest_text
         FROM ride_requests
         WHERE status = 'pending'
           AND matched_driver = ?1
           AND created_at > ?2
         ORDER BY created_at DESC LIMIT 5"
    )
    .bind(&auth.public_key)
    .bind(cutoff)
    .fetch_all(&state.db).await
    .map_err(|e| { tracing::error!("[poll] DB query failed: {}", e); e })
    .unwrap_or_default();
    let bookings: Vec<serde_json::Value> = rows.iter().map(|(id, rider, fare, ts, pickup, dest)| {
        serde_json::json!({"rideId": id, "riderPubkey": rider, "fareSats": fare, "createdAt": ts, "pickup": pickup, "destination": dest})
    }).collect();
    Ok(Json(bookings))
}

#[derive(Deserialize)]
pub struct AcceptBookingInput {
    pub ride_id: String,
    pub eta_min: Option<i64>,
    pub fare_sats: Option<i64>,
}

pub async fn accept_booking_http(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<AcceptBookingInput>,
) -> AppResult<Json<serde_json::Value>> {
    // SECURITY: this used to be a permissive UPDATE with no state check, no driver eligibility,
    // no self-booking guard, and silently swallowed errors. Now it enforces all the same checks as accept_ride.
    let now = chrono::Utc::now().timestamp();

    let driver_lud16: Option<String> = sqlx::query_scalar(
        "SELECT lud16 FROM driver_locations WHERE pubkey=?1"
    ).bind(&auth.public_key).fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;
    let driver_lud16 = driver_lud16.unwrap_or_default();
    if driver_lud16.is_empty() || !driver_lud16.contains('@') {
        return Err(AppError::BadRequest(
            "driver has no valid lightning address — set one in profile before accepting rides".into()
        ));
    }

    let rider_pubkey: Option<String> = sqlx::query_scalar(
        "SELECT rider_pubkey FROM ride_requests WHERE id=?1"
    ).bind(&body.ride_id).fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;
    let rider_pubkey = rider_pubkey.ok_or_else(||
        AppError::BadRequest("ride not found".into())
    )?;
    if auth.public_key == rider_pubkey {
        tracing::warn!("[booking] accept REJECTED: caller={} tried to accept own ride={}",
            &auth.public_key[..8.min(auth.public_key.len())], &body.ride_id);
        return Err(AppError::BadRequest(
            "cannot accept your own ride request".into()
        ));
    }

    let rows = sqlx::query(
        "UPDATE ride_requests SET status='accepted', matched_driver=?1, updated_at=?2 WHERE id=?3 AND status='searching'"
    )
    .bind(&auth.public_key).bind(now).bind(&body.ride_id)
    .execute(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    if rows.rows_affected() == 0 {
        return Err(AppError::BadRequest("Ride already taken or expired".into()));
    }

    // SECURITY: Recompute fare from current driver price (matches accept_ride logic).
    let ride_meta: Option<(String, Option<f64>)> = sqlx::query_as::<_, (String, Option<f64>)>(
        "SELECT rider_pubkey, estimated_km FROM ride_requests WHERE id=?1"
    ).bind(&body.ride_id).fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;
    let (rider_pk, estimated_km_opt) = ride_meta.unwrap_or((String::new(), None));

    let mut driver_ppk: i64 = sqlx::query_scalar(
        "SELECT price_per_km FROM driver_listings WHERE driver_pubkey=?1 AND deleted_at IS NULL ORDER BY updated_at DESC LIMIT 1"
    ).bind(&auth.public_key).fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?
    .unwrap_or(0);
    if driver_ppk <= 0 {
        driver_ppk = sqlx::query_scalar::<_, i64>(
            "SELECT price_per_km FROM driver_locations WHERE pubkey=?1"
        ).bind(&auth.public_key).fetch_optional(&state.db).await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?
        .unwrap_or(0);
    }
    let estimated_km = estimated_km_opt.unwrap_or(0.0);
    let requested_fare = body.fare_sats.unwrap_or(0);
    let recomputed_fare: i64 = if driver_ppk > 0 && estimated_km > 0.0 {
        (driver_ppk as f64 * estimated_km).round() as i64
    } else {
        requested_fare
    };
    if recomputed_fare != requested_fare && recomputed_fare > 0 {
        tracing::info!("[booking] fare recomputed: ride={} driver={} requested={} -> actual={} (ppk={} km={})",
            &body.ride_id, &auth.public_key[..8.min(auth.public_key.len())],
            requested_fare, recomputed_fare, driver_ppk, estimated_km);
        let _ = sqlx::query(
            "UPDATE ride_requests SET fare_sats=?1, updated_at=?2 WHERE id=?3"
        ).bind(recomputed_fare).bind(now).bind(&body.ride_id)
        .execute(&state.db).await;
    }
    let final_fare = if recomputed_fare > 0 { recomputed_fare } else { requested_fare };

    // PHASE 2 FIX: re-validate the final fare after recompute. Even if the rider's
    // request_ride passed min_fare check, the driver's current price × km may have
    // produced a below-threshold fare (driver lowered price between request and accept).
    // Reject the accept and roll back the status='accepted' UPDATE so the ride goes
    // back to searching and the rider isn't stuck on an unfundable ride.
    let min_fare = state.cfg.min_fare_sats;
    if final_fare < min_fare {
        tracing::warn!("[booking] accept REJECTED: ride={} driver={} final_fare={} < min_fare={}",
            &body.ride_id, &auth.public_key[..8.min(auth.public_key.len())], final_fare, min_fare);
        let _ = sqlx::query(
            "UPDATE ride_requests SET status='searching', matched_driver=NULL, updated_at=?1 WHERE id=?2"
        ).bind(now).bind(&body.ride_id)
        .execute(&state.db).await;
        return Err(AppError::BadRequest(format!(
            "Computed fare {} sats is below the minimum of {} sats. Increase your price per km or estimated distance.",
            final_fare, min_fare
        )));
    }


    // Notify rider with the ACTUAL fare so they see correct price before paying
    if !rider_pk.is_empty() {
        let accept_msg = serde_json::json!({
            "to": rider_pk,
            "from": "server",
            "type": "ulendo-ride-accepted",
            "payload": {
                "rideId": body.ride_id.clone(),
                "driverPubkey": auth.public_key.clone(),
                "fareSats": final_fare,
            },
        });
        let reg = state.ws.lock().await;
        if let Some(tx) = reg.get(&rider_pk) {
            let _ = tx.send(accept_msg.to_string());
        }
    }

    tracing::info!("[booking] driver {} accepted ride {} (eta_min={:?}, fare_sats={})",
        &auth.public_key[..8.min(auth.public_key.len())], &body.ride_id, body.eta_min, final_fare);
    Ok(Json(serde_json::json!({"ok": true, "ride_id": body.ride_id, "fare_sats": final_fare})))
}

pub async fn poll_ride_status(
    auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Path(ride_id): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let row: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT id, status, fare_sats FROM ride_requests WHERE id=?1 AND rider_pubkey=?2"
    ).bind(&ride_id).bind(&auth.public_key)
    .fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;
    match row {
        Some((id, status, fare)) => Ok(Json(serde_json::json!({"ride_id": id, "status": status, "fare_sats": fare}))),
        None => Ok(Json(serde_json::json!({"ride_id": ride_id, "status": "not_found"}))),
    }
}
