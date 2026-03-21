/// NIP-98 HTTP Auth
///
/// Authorization: Nostr <base64url-encoded-kind-27235-event-json>
///
/// The event must:
///   - have kind 27235
///   - have a "u" tag matching the exact request URL
///   - have a "method" tag matching the HTTP method (uppercase)
///   - have created_at within ±60 seconds of now
///   - have a valid Schnorr signature over the event id

use axum::{
    async_trait,
    extract::FromRequestParts,
    http::{request::Parts, HeaderMap},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use secp256k1::{schnorr::Signature, Message, XOnlyPublicKey, Secp256k1};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use crate::error::AppError;

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub npub:       String,
    pub public_key: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Nip98Event {
    id:         String,
    pubkey:     String,
    created_at: i64,
    kind:       u32,
    tags:       Vec<Vec<String>>,
    content:    String,
    sig:        String,
}

impl Nip98Event {
    fn verify_signature(&self) -> Result<(), AppError> {
        let secp = Secp256k1::verification_only();

        // Recompute event id
        let serialised = serde_json::to_string(&serde_json::json!([
            0,
            self.pubkey,
            self.created_at,
            self.kind,
            self.tags,
            self.content,
        ])).map_err(|e| AppError::Unauthorized(format!("serialise error: {e}")))?;

        let hash = Sha256::digest(serialised.as_bytes());
        if hex::encode(hash) != self.id {
            return Err(AppError::Unauthorized("event id mismatch".into()));
        }

        let pubkey_bytes = hex::decode(&self.pubkey)
            .map_err(|_| AppError::Unauthorized("invalid pubkey hex".into()))?;
        let xonly = XOnlyPublicKey::from_slice(&pubkey_bytes)
            .map_err(|_| AppError::Unauthorized("invalid xonly pubkey".into()))?;

        let sig_bytes = hex::decode(&self.sig)
            .map_err(|_| AppError::Unauthorized("invalid sig hex".into()))?;
        let sig = Signature::from_slice(&sig_bytes)
            .map_err(|_| AppError::Unauthorized("invalid schnorr sig".into()))?;

        let id_bytes = hex::decode(&self.id)
            .map_err(|_| AppError::Unauthorized("invalid event id hex".into()))?;
        let msg = Message::from_digest_slice(&id_bytes)
            .map_err(|_| AppError::Unauthorized("invalid message".into()))?;

        secp.verify_schnorr(&sig, &msg, &xonly)
            .map_err(|_| AppError::Unauthorized("signature verification failed".into()))?;

        Ok(())
    }

    fn get_tag(&self, name: &str) -> Option<&str> {
        self.tags.iter()
            .find(|t| t.first().map(String::as_str) == Some(name))
            .and_then(|t| t.get(1).map(String::as_str))
    }
}

pub fn verify_nip98_header(
    headers: &HeaderMap,
    method: &str,
    url: &str,
) -> Result<(String, String), AppError> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Unauthorized("missing Authorization header".into()))?;

    let token = auth.strip_prefix("Nostr ")
        .ok_or_else(|| AppError::Unauthorized("Authorization must start with 'Nostr '".into()))?;

    let json_bytes = STANDARD.decode(token)
        .map_err(|_| AppError::Unauthorized("invalid base64".into()))?;

    let event: Nip98Event = serde_json::from_slice(&json_bytes)
        .map_err(|_| AppError::Unauthorized("invalid event JSON".into()))?;

    if event.kind != 27235 {
        return Err(AppError::Unauthorized(format!("expected kind 27235, got {}", event.kind)));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if (now - event.created_at).abs() > 60 {
        return Err(AppError::Unauthorized("event timestamp expired".into()));
    }

    let event_url = event.get_tag("u")
        .ok_or_else(|| AppError::Unauthorized("missing 'u' tag".into()))?;
    if event_url != url {
        return Err(AppError::Unauthorized(format!(
            "url mismatch: expected '{url}', got '{event_url}'"
        )));
    }

    let event_method = event.get_tag("method")
        .ok_or_else(|| AppError::Unauthorized("missing 'method' tag".into()))?;
    if !event_method.eq_ignore_ascii_case(method) {
        return Err(AppError::Unauthorized("method mismatch".into()));
    }

    event.verify_signature()?;

    // Encode as npub using bech32
    let npub = pubkey_to_npub(&event.pubkey)
        .map_err(|e| AppError::Unauthorized(format!("npub encode failed: {e}")))?;

    Ok((npub, event.pubkey.clone()))
}

fn pubkey_to_npub(hex_pubkey: &str) -> anyhow::Result<String> {
    use bech32::{Bech32, Hrp};
    let bytes = hex::decode(hex_pubkey)?;
    let hrp = Hrp::parse("npub").map_err(|e| anyhow::anyhow!("{e}"))?;
    let encoded = bech32::encode::<Bech32>(hrp, &bytes)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(encoded)
}

#[async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let method = parts.method.as_str();
        let uri    = &parts.uri;
        let url    = format!(
            "{}{}",
            uri.path(),
            uri.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        let (npub, public_key) = verify_nip98_header(&parts.headers, method, &url)?;
        Ok(AuthUser { npub, public_key })
    }
}