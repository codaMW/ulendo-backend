// ─── ULENDO CALLS: PSTN BRIDGE + CREDIT SYSTEM ───────────────────────────────
// Talk credits funded by Bitcoin/Lightning.
// PSTN bridge via Africa's Talking Voice API.
// npub→local: deducts credits per minute.
// local→npub: inbound webhook rings the target npub via WebSocket.

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use crate::{auth::AuthUser, error::{AppError, AppResult}, AppState};

const AT_VOICE_URL:         &str = "https://voice.africastalking.com/call";
const AT_VOICE_SANDBOX_URL: &str = "https://voice.sandbox.africastalking.com/call";

// ─── TYPES ───────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct BalanceResp {
    pub balance_sats: i64,
    pub sats_per_minute: i64,
    pub minutes_remaining: i64,
}

#[derive(Deserialize)]
pub struct TopupBody {
    pub amount_sats: i64,
}

#[derive(Serialize)]
pub struct TopupResp {
    pub invoice:      String,
    pub payment_hash: String,
    pub amount_sats:  i64,
    pub topup_id:     String,
}

#[derive(Deserialize)]
pub struct OutboundBody {
    pub phone:   String,   // E.164 format e.g. +265991234567
    pub call_id: String,   // frontend-generated UUID for correlation
}

#[derive(Serialize)]
pub struct OutboundResp {
    pub ok:            bool,
    pub at_session_id: Option<String>,
    pub message:       String,
}

#[derive(Deserialize)]
pub struct AtWebhookQuery {
    pub sessionId:    Option<String>,
    pub callSessionState: Option<String>,
    pub duration:     Option<String>,
    pub callerNumber: Option<String>,
    pub destinationNumber: Option<String>,
    pub direction:    Option<String>,
}

// ─── HELPERS ─────────────────────────────────────────────────────────────────

async fn get_or_create_balance(db: &sqlx::SqlitePool, pubkey: &str) -> Result<i64, AppError> {
    let now = chrono::Utc::now().timestamp();
    // Upsert — create row with 0 balance if not exists
    sqlx::query(
        "INSERT INTO call_credits (pubkey, balance_sats, updated_at)
         VALUES (?1, 0, ?2)
         ON CONFLICT(pubkey) DO NOTHING"
    )
    .bind(pubkey).bind(now)
    .execute(db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let bal: i64 = sqlx::query_scalar(
        "SELECT balance_sats FROM call_credits WHERE pubkey=?1"
    )
    .bind(pubkey)
    .fetch_one(db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    Ok(bal)
}

// ─── HANDLERS ────────────────────────────────────────────────────────────────

// GET /calls/balance
pub async fn get_balance(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<BalanceResp>> {
    let balance_sats = get_or_create_balance(&state.db, &auth.public_key).await?;
    let spm = state.cfg.sats_per_minute_pstn;
    let minutes = if spm > 0 { balance_sats / spm } else { 0 };
    Ok(Json(BalanceResp {
        balance_sats,
        sats_per_minute: spm,
        minutes_remaining: minutes,
    }))
}

// POST /calls/topup  { amount_sats }
// Creates a Lightning invoice. Frontend polls /calls/topup/:id/status.
pub async fn create_topup(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<TopupBody>,
) -> AppResult<Json<TopupResp>> {
    if body.amount_sats < 100 {
        return Err(AppError::BadRequest("Minimum top-up is 100 sats".into()));
    }
    if body.amount_sats > 1_000_000 {
        return Err(AppError::BadRequest("Maximum top-up is 1,000,000 sats".into()));
    }

    let memo = format!("Ulendo talk credits {} sats", body.amount_sats);
    let inv = state.blink
        .create_invoice(body.amount_sats, &memo).await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Invoice error: {e}")))?;

    let id  = uuid::Uuid::new_v4().to_string().replace('-', "");
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO call_topups (id, pubkey, invoice, payment_hash, amount_sats, paid, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)"
    )
    .bind(&id)
    .bind(&auth.public_key)
    .bind(&inv.payment_request)
    .bind(&inv.payment_hash)
    .bind(body.amount_sats)
    .bind(now)
    .execute(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    tracing::info!("[calls] topup created: pubkey={} sats={}", &auth.public_key[..8], body.amount_sats);
    Ok(Json(TopupResp {
        invoice:      inv.payment_request,
        payment_hash: inv.payment_hash,
        amount_sats:  body.amount_sats,
        topup_id:     id,
    }))
}

// GET /calls/topup/:id/status
// Poll to check if invoice was paid. If paid, credit the account.
pub async fn topup_status(
    auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let row = sqlx::query_as::<_, (String, i64, i64, String)>(
        "SELECT pubkey, amount_sats, paid, invoice FROM call_topups WHERE id=?1"
    )
    .bind(&id)
    .fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?
    .ok_or_else(|| AppError::BadRequest("Topup not found".into()))?;

    let (pubkey, amount_sats, paid, invoice_str) = row;
    if pubkey != auth.public_key {
        return Err(AppError::Unauthorized("Not your topup".into()));
    }
    if paid == 1 {
        return Ok(Json(serde_json::json!({"paid": true, "amount_sats": amount_sats})));
    }

    // Check payment with Blink
    let confirmed = state.blink.is_invoice_paid(&invoice_str).await.unwrap_or(false);
    if confirmed {
        let now = chrono::Utc::now().timestamp();
        // Mark paid
        sqlx::query("UPDATE call_topups SET paid=1 WHERE id=?1")
            .bind(&id).execute(&state.db).await.ok();
        // Credit balance
        sqlx::query(
            "INSERT INTO call_credits (pubkey, balance_sats, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(pubkey) DO UPDATE SET
               balance_sats = balance_sats + ?2,
               updated_at   = ?3"
        )
        .bind(&pubkey).bind(amount_sats).bind(now)
        .execute(&state.db).await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("DB credit error: {e}")))?;

        tracing::info!("[calls] topup paid: pubkey={} sats={}", &pubkey[..8], amount_sats);
        return Ok(Json(serde_json::json!({"paid": true, "amount_sats": amount_sats})));
    }

    Ok(Json(serde_json::json!({"paid": false})))
}

// POST /calls/pstn/outbound  { phone, call_id }
// Initiates a call from our AT number to a local Malawi number.
// Deducts credits upfront for 1 minute; remainder adjusted via webhook.
pub async fn pstn_outbound(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<OutboundBody>,
) -> AppResult<Json<OutboundResp>> {
    // Validate phone
    let phone = body.phone.trim().to_string();
    if !phone.starts_with('+') || phone.len() < 8 {
        return Err(AppError::BadRequest("Phone must be in E.164 format e.g. +265991234567".into()));
    }

    // Check balance — require at least 1 minute
    let balance = get_or_create_balance(&state.db, &auth.public_key).await?;
    let spm = state.cfg.sats_per_minute_pstn;
    if balance < spm {
        return Err(AppError::BadRequest(
            format!("Insufficient credits. Need at least {} sats ({} sats/min). Top up first.", spm, spm)
        ));
    }

    let now = chrono::Utc::now().timestamp();
    let log_id = uuid::Uuid::new_v4().to_string().replace('-', "");

    // Insert PSTN log entry (duration/sats updated via webhook)
    sqlx::query(
        "INSERT INTO call_pstn_log (id, pubkey, direction, phone, duration_secs, sats_spent, status, created_at)
         VALUES (?1, ?2, 'outbound', ?3, 0, 0, 'initiated', ?4)"
    )
    .bind(&log_id).bind(&auth.public_key).bind(&phone).bind(now)
    .execute(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    // Call Africa's Talking Voice API
    let at_url = if state.cfg.at_sandbox { AT_VOICE_SANDBOX_URL } else { AT_VOICE_URL };
    let from   = &state.cfg.at_phone_number;
    let client = reqwest::Client::new();

    let resp = client.post(at_url)
        .header("apiKey", &state.cfg.at_api_key)
        .header("Accept", "application/json")
        .form(&[
            ("username", state.cfg.at_username.as_str()),
            ("from",     from.as_str()),
            ("to",       phone.as_str()),
        ])
        .send().await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("AT API error: {e}")))?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    tracing::info!("[calls] AT outbound: status={} body={}", status, &body_text[..body_text.len().min(200)]);

    if !status.is_success() {
        return Ok(Json(OutboundResp {
            ok: false,
            at_session_id: None,
            message: format!("AT API error: {}", body_text),
        }));
    }

    // Parse session ID from AT response
    let at_session_id: Option<String> = serde_json::from_str::<serde_json::Value>(&body_text)
        .ok()
        .and_then(|v| v["entries"][0]["sessionId"].as_str().map(|s| s.to_string()));

    if let Some(ref sid) = at_session_id {
        sqlx::query("UPDATE call_pstn_log SET at_session_id=?2, status='ringing' WHERE id=?1")
            .bind(&log_id).bind(sid)
            .execute(&state.db).await.ok();
    }

    Ok(Json(OutboundResp {
        ok: true,
        at_session_id,
        message: "Call initiated".into(),
    }))
}

// POST /calls/pstn/webhook  (Africa's Talking calls this)
// Handles call state events: answered, completed, etc.
// On completion: calculate cost, deduct from balance.
pub async fn pstn_webhook(
    State(state): State<AppState>,
    axum::extract::Form(q): axum::extract::Form<AtWebhookQuery>,
) -> axum::response::Response {
    let session_id = q.sessionId.as_deref().unwrap_or("");
    let call_state = q.callSessionState.as_deref().unwrap_or("");
    let duration   = q.duration.as_deref().unwrap_or("0").parse::<i64>().unwrap_or(0);

    tracing::info!("[calls] AT webhook: session={} state={} duration={}",
        session_id, call_state, duration);

    if call_state == "Complete" && !session_id.is_empty() {
        let now = chrono::Utc::now().timestamp();
        // Find log entry
        let row = sqlx::query_as::<_, (String, String)>(
            "SELECT id, pubkey FROM call_pstn_log WHERE at_session_id=?1"
        )
        .bind(session_id)
        .fetch_optional(&state.db).await;

        if let Ok(Some((log_id, pubkey))) = row {
            let spm       = state.cfg.sats_per_minute_pstn;
            let minutes   = (duration as f64 / 60.0).ceil() as i64;
            let sats_cost = (minutes * spm).max(spm); // minimum 1 minute charge

            // Update log
            sqlx::query(
                "UPDATE call_pstn_log SET duration_secs=?2, sats_spent=?3, status='completed' WHERE id=?1"
            )
            .bind(&log_id).bind(duration).bind(sats_cost)
            .execute(&state.db).await.ok();

            // Deduct from balance (floor at 0)
            sqlx::query(
                "UPDATE call_credits SET
                   balance_sats = MAX(0, balance_sats - ?2),
                   updated_at   = ?3
                 WHERE pubkey=?1"
            )
            .bind(&pubkey).bind(sats_cost).bind(now)
            .execute(&state.db).await.ok();

            tracing::info!("[calls] PSTN complete: session={} secs={} sats={}", session_id, duration, sats_cost);
        }
    }

    // AT expects XML or empty 200 response
    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "text/xml")
        .body(axum::body::Body::from(""))
        .unwrap()
}

// POST /calls/pstn/inbound  (Africa's Talking calls this for incoming calls)
// A local Malawi number called our AT number.
// Returns XML telling AT what to do (we handle routing via WS separately).
pub async fn pstn_inbound(
    State(state): State<AppState>,
    axum::extract::Form(q): axum::extract::Form<AtWebhookQuery>,
) -> axum::response::Response {
    let caller = q.callerNumber.as_deref().unwrap_or("unknown");
    let dest   = q.destinationNumber.as_deref().unwrap_or("");
    tracing::info!("[calls] AT inbound: from={} to={}", caller, dest);

    // TODO Phase 3: look up target npub by dest number, push WS ring event
    // For now: play a message and hang up
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Say voice="woman">Welcome to Ulendo. This service is coming soon. Goodbye.</Say>
</Response>"#;

    axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "text/xml")
        .body(axum::body::Body::from(xml))
        .unwrap()
}
