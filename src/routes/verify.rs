use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::{AppError, AppResult, AppState};

#[derive(Deserialize)]
pub struct CreateVerifyInvoiceRequest {
    pub tier:   String,
    pub npub:   String,
}

#[derive(Serialize)]
pub struct CreateVerifyInvoiceResponse {
    pub payment_request: String,
    pub payment_hash:    String,
    pub tier:            String,
    pub sats:            i64,
}

pub async fn create_invoice(
    State(state): State<AppState>,
    Json(body): Json<CreateVerifyInvoiceRequest>,
) -> AppResult<Json<CreateVerifyInvoiceResponse>> {
    let sats: i64 = match body.tier.as_str() {
        "silver" => 6000,
        "gold"   => 12000,
        "orange" => 27000,
        _ => return Err(AppError::BadRequest("invalid tier".into())),
    };

    let memo = format!("Ulendo {} verification for {}", body.tier, body.npub);
    let inv = state.blink.create_invoice(sats, &memo).await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    Ok(Json(CreateVerifyInvoiceResponse {
        payment_request: inv.payment_request,
        payment_hash:    inv.payment_hash,
        tier:            body.tier,
        sats,
    }))
}

#[derive(Deserialize)]
pub struct CheckVerifyInvoiceRequest {
    pub payment_request: String,
}

pub async fn check_invoice(
    State(state): State<AppState>,
    Json(body): Json<CheckVerifyInvoiceRequest>,
) -> AppResult<Json<Value>> {
    let paid = state.blink.is_invoice_paid(&body.payment_request).await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(json!({ "paid": paid })))
}
