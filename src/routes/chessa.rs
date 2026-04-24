use axum::{
    extract::{Path, State},
    Json,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{error::{AppError, AppResult}, AppState};

const CHESSA_BASE: &str = "https://api.chessa.ai";

fn client_creds() -> (String, String) {
    (
        std::env::var("CHESSA_CLIENT_ID").unwrap_or_default(),
        std::env::var("CHESSA_CLIENT_SECRET").unwrap_or_default(),
    )
}

async fn chessa_post(path: &str, body: Value) -> AppResult<Value> {
    let (id, secret) = client_creds();
    let url = format!("{}{}", CHESSA_BASE, path);
    let resp = Client::new()
        .post(&url)
        .header("x-client-id", id)
        .header("x-client-secret", secret)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let status = resp.status();
    let json: Value = resp.json().await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    if !status.is_success() {
        let msg = json["message"].as_str().unwrap_or("Chessa error").to_string();
        return Err(AppError::BadRequest(msg));
    }
    Ok(json)
}

async fn chessa_get(path: &str) -> AppResult<Value> {
    let (id, secret) = client_creds();
    let url = format!("{}{}", CHESSA_BASE, path);
    let resp = Client::new()
        .get(&url)
        .header("x-client-id", id)
        .header("x-client-secret", secret)
        .send()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    let json: Value = resp.json().await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    Ok(json)
}

#[derive(Deserialize)]
pub struct QuoteRequest {
    pub booking_id: String,
    pub fare_sats:  i64,
    pub phone:      String,
    pub network:    String,
    pub first_name: String,
    pub last_name:  String,
}

#[derive(Serialize)]
pub struct QuoteResponse {
    pub order_id:         String,
    pub recipient_id:     String,
    pub mwk_amount:       f64,
    pub btc_to_mwk_rate:  f64,
    pub fee_amount:       f64,
    pub network_label:    String,
    pub quote_expires_at: String,
}

#[derive(Deserialize)]
pub struct PayRequest {
    pub booking_id: String,
    pub order_id:   String,
    pub fare_sats:  i64,
}

#[derive(Serialize)]
pub struct PayResponse {
    pub order_id:       String,
    pub status:         String,
    pub crypto_address: String,
}

pub async fn create_quote(
    Json(req): Json<QuoteRequest>,
) -> AppResult<Json<QuoteResponse>> {
    let recipient_resp = chessa_post("/v1/recipient", json!({
        "firstName":   req.first_name,
        "lastName":    req.last_name,
        "phoneNumber": req.phone,
        "country":     "MW",
        "payoutMethod": "mobile_money",
        "mobileMoneyDetails": {
            "provider":    req.network,
            "phoneNumber": req.phone,
            "country":     "MW",
            "currency":    "MWK"
        }
    })).await?;

    let recipient_id = recipient_resp["recipientId"]
        .as_str().unwrap_or("").to_string();

    let btc_amount = req.fare_sats as f64 / 100_000_000.0;
    let order_resp = chessa_post("/v1/orders", json!({
        "recipientId":         recipient_id,
        "sourceCurrency":      "BTC",
        "sourceAmount":        btc_amount,
        "destinationCurrency": "MWK",
        "memo": format!("Ulendo ride payout booking {}", req.booking_id),
    })).await?;

    let network_label = match req.network.as_str() {
        "airtel_mw"  => "Airtel Money",
        "tnm_mpamba" => "TNM Mpamba",
        other        => other,
    }.to_string();

    Ok(Json(QuoteResponse {
        order_id:         order_resp["orderId"].as_str().unwrap_or("").to_string(),
        recipient_id,
        mwk_amount:       order_resp["destinationAmount"].as_f64().unwrap_or(0.0),
        btc_to_mwk_rate:  order_resp["exchangeRate"].as_f64().unwrap_or(0.0),
        fee_amount:       order_resp["fee"].as_f64().unwrap_or(0.0),
        network_label,
        quote_expires_at: order_resp["expiresAt"].as_str().unwrap_or("").to_string(),
    }))
}

pub async fn pay_order(
    State(state): State<AppState>,
    Json(req): Json<PayRequest>,
) -> AppResult<Json<PayResponse>> {
    let funding_resp = chessa_post(
        "/v1/orders/funding",
        json!({ "orderId": req.order_id }),
    ).await?;

    let crypto_address = funding_resp["cryptoAddress"]
        .as_str().unwrap_or("").to_string();

    sqlx::query!(
        "UPDATE bookings SET chessa_order_id = ?, chessa_crypto_address = ?, payout_choice = ? WHERE id = ?",
        req.order_id,
        crypto_address,
        "kwacha",
        req.booking_id,
    )
    .execute(&state.db)
    .await?;

    tracing::info!(
        booking = %req.booking_id,
        order   = %req.order_id,
        address = %crypto_address,
        "Chessa payout pending"
    );

    Ok(Json(PayResponse {
        order_id: req.order_id,
        status:   "pending".to_string(),
        crypto_address,
    }))
}

pub async fn get_order_status(
    Path(order_id): Path<String>,
) -> AppResult<Json<Value>> {
    let resp = chessa_get(&format!("/v1/orders/{}", order_id)).await?;
    Ok(Json(json!({
        "orderId":           order_id,
        "status":            resp["status"],
        "destinationAmount": resp["destinationAmount"],
        "updatedAt":         resp["updatedAt"],
    })))
}

pub async fn get_config() -> AppResult<Json<Value>> {
    let resp = chessa_get("/v1/configurations").await?;
    Ok(Json(resp))
}
