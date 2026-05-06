use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use crate::{AppState, auth::AuthUser, error::{AppError, AppResult}};

const FIAT_COMMISSION_PCT: i64 = 13;

fn ulendo_airtel() -> String {
    std::env::var("ULENDO_AIRTEL_NUMBER").unwrap_or_else(|_| "+265991234567".into())
}
fn ulendo_tnm() -> String {
    std::env::var("ULENDO_TNM_NUMBER").unwrap_or_else(|_| "+265881234567".into())
}

#[derive(Deserialize)]
pub struct CreateFiatEscrow {
    pub ride_id: String,
    pub driver_pubkey: String,
    pub driver_phone: Option<String>,
    pub amount_mwk: i64,
}

#[derive(Serialize)]
pub struct FiatEscrowCreated {
    pub escrow_id: String,
    pub reference_code: String,
    pub amount_mwk: i64,
    pub total_with_fee: i64,
    pub commission_mwk: i64,
    pub driver_payout: i64,
    pub airtel_number: String,
    pub tnm_number: String,
}

pub async fn create_fiat_escrow(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<CreateFiatEscrow>,
) -> AppResult<Json<FiatEscrowCreated>> {
    if body.amount_mwk < 100 {
        return Err(AppError::BadRequest("Minimum amount is MWK 100".into()));
    }
    let id = uuid::Uuid::new_v4().to_string().replace('-', "");
    let ref_code = format!("UL-{}", &id[..6].to_uppercase());
    let commission = body.amount_mwk * FIAT_COMMISSION_PCT / 100;
    let total_with_fee = body.amount_mwk + commission;
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO fiat_escrow (id,ride_id,rider_pubkey,driver_pubkey,amount_mwk,commission_mwk,driver_payout,driver_phone,reference_code,status,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'pending',?10)"
    )
    .bind(&id).bind(&body.ride_id).bind(&auth.public_key).bind(&body.driver_pubkey)
    .bind(total_with_fee).bind(commission).bind(body.amount_mwk)
    .bind(body.driver_phone.as_deref().unwrap_or("")).bind(&ref_code).bind(now)
    .execute(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;

    tracing::info!("[fiat] escrow {} created: MWK {} + {} fee", ref_code, body.amount_mwk, commission);

    Ok(Json(FiatEscrowCreated {
        escrow_id: id, reference_code: ref_code,
        amount_mwk: body.amount_mwk, total_with_fee, commission_mwk: commission,
        driver_payout: body.amount_mwk,
        airtel_number: ulendo_airtel(), tnm_number: ulendo_tnm(),
    }))
}

#[derive(Deserialize)]
pub struct VerifySmsInput {
    pub escrow_id: String,
    pub sms_text: String,
}

#[derive(Serialize)]
pub struct VerifyResult {
    pub verified: bool,
    pub message: String,
}

pub async fn verify_sms(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<VerifySmsInput>,
) -> AppResult<Json<VerifyResult>> {
    let escrow: Option<(String, String, i64, String)> = sqlx::query_as(
        "SELECT id, driver_pubkey, amount_mwk, status FROM fiat_escrow WHERE id=?1 AND rider_pubkey=?2"
    ).bind(&body.escrow_id).bind(&auth.public_key)
    .fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;

    let (esc_id, driver_pk, expected, status) = match escrow {
        Some(e) => e,
        None => return Ok(Json(VerifyResult { verified: false, message: "Escrow not found".into() })),
    };
    if status != "pending" {
        return Ok(Json(VerifyResult { verified: false, message: "Already funded".into() }));
    }

    let parsed = parse_sms(&body.sms_text);
    if parsed.amount == 0 {
        return Ok(Json(VerifyResult { verified: false, message: "Could not read amount from SMS. Paste the full confirmation message.".into() }));
    }
    if parsed.amount < expected {
        return Ok(Json(VerifyResult { verified: false, message: format!("Amount too low. Expected MWK {}, found MWK {}", expected, parsed.amount) }));
    }

    if !parsed.txn_ref.is_empty() {
        let used: bool = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM used_txn_refs WHERE txn_ref=?1)")
            .bind(&parsed.txn_ref).fetch_one(&state.db).await.unwrap_or(false);
        if used {
            return Ok(Json(VerifyResult { verified: false, message: "This transaction was already used.".into() }));
        }
        sqlx::query("INSERT OR IGNORE INTO used_txn_refs (txn_ref, escrow_id) VALUES (?1,?2)")
            .bind(&parsed.txn_ref).bind(&esc_id).execute(&state.db).await.ok();
    }

    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE fiat_escrow SET status='funded', sms_raw=?1, txn_ref=?2, funded_at=?3 WHERE id=?4")
        .bind(&body.sms_text).bind(&parsed.txn_ref).bind(now).bind(&esc_id)
        .execute(&state.db).await.ok();

    // Notify driver
    let msg = serde_json::json!({"to":driver_pk,"from":"server","type":"ulendo-fiat-funded","payload":{"escrowId":esc_id,"amountMwk":expected}});
    { let reg = state.ws.lock().await; if let Some(tx) = reg.get(&driver_pk) { let _ = tx.send(msg.to_string()); } }

    tracing::info!("[fiat] escrow {} FUNDED: MWK {} ref={}", esc_id, expected, parsed.txn_ref);
    Ok(Json(VerifyResult { verified: true, message: "Payment verified! Driver notified.".into() }))
}

#[derive(Deserialize)]
pub struct ReleaseFiatInput { pub escrow_id: String }

#[derive(Serialize)]
pub struct ReleaseResult { pub status: String, pub driver_payout: i64, pub commission: i64 }

pub async fn release_fiat(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<ReleaseFiatInput>,
) -> AppResult<Json<ReleaseResult>> {
    let now = chrono::Utc::now().timestamp();
    let escrow: Option<(String, String, i64, i64, String)> = sqlx::query_as(
        "SELECT id, driver_pubkey, driver_payout, commission_mwk, driver_phone FROM fiat_escrow
         WHERE id=?1 AND (rider_pubkey=?2 OR driver_pubkey=?2) AND status IN ('funded','in_ride')"
    ).bind(&body.escrow_id).bind(&auth.public_key)
    .fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;

    let (esc_id, driver_pk, payout, commission, phone) = match escrow {
        Some(e) => e,
        None => return Err(AppError::NotFound("Escrow not found or already released".into())),
    };

    sqlx::query("UPDATE fiat_escrow SET status='released', completed_at=?1, released_at=?1 WHERE id=?2")
        .bind(now).bind(&esc_id).execute(&state.db).await.ok();

    let pid = uuid::Uuid::new_v4().to_string().replace('-', "");
    sqlx::query("INSERT INTO driver_payouts (id,driver_pubkey,driver_phone,amount_mwk,ride_id,escrow_id,status,created_at) VALUES (?1,?2,?3,?4,?5,?5,'pending',?6)")
        .bind(&pid).bind(&driver_pk).bind(&phone).bind(payout).bind(&esc_id).bind(now)
        .execute(&state.db).await.ok();

    sqlx::query("INSERT INTO driver_stats (pubkey,total_rides,total_earned_mwk,updated_at) VALUES (?1,1,?2,?3) ON CONFLICT(pubkey) DO UPDATE SET total_rides=driver_stats.total_rides+1, total_earned_mwk=driver_stats.total_earned_mwk+?2, updated_at=?3")
        .bind(&driver_pk).bind(payout).bind(now).execute(&state.db).await.ok();

    let msg = serde_json::json!({"to":driver_pk,"from":"server","type":"ulendo-fiat-released","payload":{"amountMwk":payout}});
    { let reg = state.ws.lock().await; if let Some(tx) = reg.get(&driver_pk) { let _ = tx.send(msg.to_string()); } }

    tracing::info!("[fiat] RELEASED: driver gets MWK {}, Ulendo keeps MWK {}", payout, commission);
    Ok(Json(ReleaseResult { status: "released".into(), driver_payout: payout, commission }))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct PendingPayout { pub id: String, pub driver_pubkey: String, pub driver_phone: String, pub amount_mwk: i64, pub status: String, pub created_at: i64 }

pub async fn pending_payouts(State(state): State<AppState>) -> AppResult<Json<Vec<PendingPayout>>> {
    let p = sqlx::query_as::<_, PendingPayout>("SELECT id,driver_pubkey,driver_phone,amount_mwk,status,created_at FROM driver_payouts WHERE status='pending' ORDER BY created_at")
        .fetch_all(&state.db).await.map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;
    Ok(Json(p))
}

struct Parsed { amount: i64, txn_ref: String }

fn parse_sms(sms: &str) -> Parsed {
    let mut r = Parsed { amount: 0, txn_ref: String::new() };
    if let Some(pat) = regex_lite::Regex::new(r"(?i)(?:MWK|MK|K)\s*([0-9,]+(?:\.[0-9]{2})?)").ok() {
        if let Some(cap) = pat.captures(sms) {
            if let Ok(v) = cap.get(1).unwrap().as_str().replace(',', "").parse::<f64>() { r.amount = v as i64; }
        }
    }
    if let Some(pat) = regex_lite::Regex::new(r"(?i)(?:Ref|Transaction\s*ID|TXN|ID)[:\s]*([A-Z0-9]{6,20})").ok() {
        if let Some(cap) = pat.captures(sms) { r.txn_ref = cap.get(1).unwrap().as_str().to_string(); }
    }
    r
}
