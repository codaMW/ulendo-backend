use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::sleep;

use crate::AppState;

pub struct BlinkClient {
    pub api_url:   String,
    pub api_key:   String,
    pub wallet_id: String,
    client:        Client,
}

impl BlinkClient {
    pub fn new(api_url: String, api_key: String, wallet_id: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client");
        Self { api_url, api_key, wallet_id, client }
    }

    async fn gql<T: for<'de> Deserialize<'de>>(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T> {
        let resp = self.client
            .post(&self.api_url)
            .header("X-API-KEY", &self.api_key)
            .json(&serde_json::json!({ "query": query, "variables": variables }))
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;

        if let Some(errors) = body.get("errors") {
            let msg = errors[0]["message"].as_str().unwrap_or("blink error");
            return Err(anyhow::anyhow!("blink: {msg}"));
        }

        let data = body["data"].clone();
        Ok(serde_json::from_value(data)?)
    }

    /// Create a standard Lightning invoice (for escrow).
    /// In production you'd use a HODL invoice — Blink's API supports this
    /// via the lnInvoiceCreateOnBehalfOfRecipient mutation when available.
    pub async fn create_invoice(
        &self,
        amount_sats: i64,
        memo: &str,
    ) -> Result<InvoiceResult> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(rename = "lnInvoiceCreate")]
            create: InvoiceCreateResult,
        }
        #[derive(Deserialize)]
        struct InvoiceCreateResult {
            invoice: Option<InvoiceData>,
            errors:  Vec<GqlError>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct InvoiceData {
            pub payment_request: String,
            pub payment_hash:    String,
        }
        #[derive(Deserialize)]
        struct GqlError { message: String }

        let resp: Resp = self.gql(
            r#"mutation LnInvoiceCreate($input: LnInvoiceCreateInput!) {
                lnInvoiceCreate(input: $input) {
                    invoice { paymentRequest paymentHash }
                    errors  { message }
                }
            }"#,
            serde_json::json!({
                "input": {
                    "walletId": self.wallet_id,
                    "amount":   amount_sats,
                    "memo":     memo,
                }
            }),
        ).await?;

        if !resp.create.errors.is_empty() {
            return Err(anyhow::anyhow!("{}", resp.create.errors[0].message));
        }

        let inv = resp.create.invoice
            .ok_or_else(|| anyhow::anyhow!("no invoice returned"))?;

        Ok(InvoiceResult {
            payment_request: inv.payment_request,
            payment_hash:    inv.payment_hash,
            expires_at:      chrono::Utc::now().timestamp() + 86400, // 24h
        })
    }

    /// Check whether a Lightning invoice has been paid.
    pub async fn is_invoice_paid(&self, payment_request: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(rename = "lnInvoicePaymentStatus")]
            status: PaymentStatus,
        }
        #[derive(Deserialize)]
        struct PaymentStatus { status: String }

        let resp: Resp = self.gql(
            r#"query LnInvoicePaymentStatus($input: LnInvoicePaymentStatusInput!) {
                lnInvoicePaymentStatus(input: $input) { status }
            }"#,
            serde_json::json!({
                "input": { "paymentRequest": payment_request }
            }),
        ).await?;

        Ok(resp.status.status == "PAID")
    }

    /// Send sats to a Lightning address (release escrow to merchant).
    pub async fn send_to_address(
        &self,
        lud16: &str,
        amount_sats: i64,
        memo: &str,
    ) -> Result<String> {
        let parts: Vec<&str> = lud16.splitn(2, '@').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!("invalid lud16: {}", lud16));
        }
        let (user, domain) = (parts[0], parts[1]);

        if domain == "blink.sv" {
            // Fast intra-ledger path
            self.send_intra_ledger(user, amount_sats, memo).await
        } else {
            // External Lightning address
            self.send_ln_address(lud16, amount_sats, memo).await
        }
    }

    async fn send_intra_ledger(&self, username: &str, amount: i64, memo: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct WalletResp {
            #[serde(rename = "userDefaultWalletId")]
            wallet_id: String,
        }
        let w: WalletResp = self.gql(
            "query Q($u: Username!) { userDefaultWalletId(username: $u) }",
            serde_json::json!({ "u": username }),
        ).await?;

        #[derive(Deserialize)]
        struct SendResp {
            #[serde(rename = "intraLedgerPaymentSend")]
            send: StatusResult,
        }
        #[derive(Deserialize)]
        struct StatusResult { status: String }

        let r: SendResp = self.gql(
            r#"mutation S($input: IntraLedgerPaymentSendInput!) {
                intraLedgerPaymentSend(input: $input) { status }
            }"#,
            serde_json::json!({
                "input": {
                    "walletId":            self.wallet_id,
                    "recipientWalletId":   w.wallet_id,
                    "amount":              amount,
                    "memo":                memo,
                }
            }),
        ).await?;
        Ok(r.send.status)
    }

    async fn send_ln_address(&self, lud16: &str, amount: i64, memo: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct SendResp {
            #[serde(rename = "lnAddressPaymentSend")]
            send: StatusResult,
        }
        #[derive(Deserialize)]
        struct StatusResult { status: String }

        let r: SendResp = self.gql(
            r#"mutation S($input: LnAddressPaymentSendInput!) {
                lnAddressPaymentSend(input: $input) { status }
            }"#,
            serde_json::json!({
                "input": {
                    "walletId":  self.wallet_id,
                    "lnAddress": lud16,
                    "amount":    amount,
                    "memo":      memo,
                }
            }),
        ).await?;
        Ok(r.send.status)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvoiceResult {
    pub payment_request: String,
    pub payment_hash:    String,
    pub expires_at:      i64,
}

/// Background task: polls all 'pending' bookings every 10 seconds.
/// When a booking's invoice is paid, transitions it to 'funded'.
pub async fn run_escrow_monitor(state: AppState) {
    tracing::info!("escrow monitor started");
    loop {
        sleep(Duration::from_secs(10)).await;

        let pending = sqlx::query_as::<_, crate::db::Booking>(
            "SELECT * FROM bookings WHERE status = 'pending' AND payment_request IS NOT NULL"
        )
        .fetch_all(&state.db)
        .await;

        let bookings = match pending {
            Ok(b) => b,
            Err(e) => { tracing::warn!("escrow monitor db error: {e}"); continue; }
        };

        for booking in bookings {
            let pr = match &booking.payment_request {
                Some(pr) => pr.clone(),
                None     => continue,
            };

            match state.blink.is_invoice_paid(&pr).await {
                Ok(true) => {
                    let now = chrono::Utc::now().timestamp();
                    let res = sqlx::query(
                        "UPDATE bookings SET status='funded', funded_at=?1, updated_at=?2
                         WHERE id=?3 AND status='pending'"
                    )
                    .bind(now)
                    .bind(now)
                    .bind(&booking.id)
                    .execute(&state.db)
                    .await;

                    if let Ok(r) = res {
                        if r.rows_affected() > 0 {
                            tracing::info!("booking {} funded", booking.id);

                            // Send push notification to booking owner
                            notify_booking_funded(&state, &booking).await;
                        }
                    }
                }
                Ok(false) => {}
                Err(e) => tracing::warn!("invoice check failed for {}: {e}", booking.id),
            }
        }
    }
}

async fn notify_booking_funded(state: &AppState, booking: &crate::db::Booking) {
    let subs = sqlx::query_as::<_, crate::db::PushSubscription>(
        "SELECT * FROM push_subscriptions WHERE npub = ?1"
    )
    .bind(&booking.booker_npub)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    for sub in subs {
        let payload = serde_json::json!({
            "title": "Escrow funded!",
            "body":  format!("Your booking is active. {} sats held.", booking.amount_sats),
            "data":  { "booking_id": booking.id, "type": "booking_funded" }
        });
        let _ = state.push.send(&sub, payload.to_string()).await;
    }
}