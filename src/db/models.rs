use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Identity {
    pub npub:       String,
    pub public_key: String,
    pub name:       Option<String>,
    pub role:       String,
    pub lud16:      Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Listing {
    pub id:             String,
    pub owner_npub:     String,
    pub nostr_event_id: Option<String>,
    pub category:       String,
    pub name:           String,
    pub area:           String,
    pub description:    Option<String>,
    pub price_sats:     i64,
    pub price_unit:     String,
    pub lud16:          Option<String>,
    pub photos_json:    String,
    pub phone:          Option<String>,
    pub available:      i64,
    pub verified:       i64,
    pub created_at:     i64,
    pub updated_at:     i64,
}

impl Listing {
    pub fn photos(&self) -> Vec<String> {
        serde_json::from_str(&self.photos_json).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Booking {
    pub id:               String,
    pub listing_id:       String,
    pub booker_npub:      String,
    pub booking_type:     String,
    pub status:           String,
    pub amount_sats:      i64,
    pub fee_sats:         i64,
    pub lud16_refund:     Option<String>,
    pub payment_hash:     Option<String>,
    pub payment_request:  Option<String>,
    pub invoice_expires_at: Option<i64>,
    pub ride_id:          Option<String>,
    pub pickup_text:      Option<String>,
    pub destination_text: Option<String>,
    pub pickup_gps_lat:   Option<f64>,
    pub pickup_gps_lng:   Option<f64>,
    pub funded_at:        Option<i64>,
    pub held_at:          Option<i64>,
    pub released_at:      Option<i64>,
    pub disputed_at:      Option<i64>,
    pub refunded_at:      Option<i64>,
    pub completed_at:     Option<i64>,
    pub rider_confirmed_at:  Option<i64>,
    pub driver_confirmed_at: Option<i64>,
    pub pickup_confirmed_at: Option<i64>,
    pub cancelled_at:     Option<i64>,
    pub created_at:       i64,
    pub updated_at:       i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PushSubscription {
    pub id:         String,
    pub npub:       String,
    pub endpoint:   String,
    pub p256dh:     String,
    pub auth:       String,
    pub platform:   Option<String>,
    pub user_agent: Option<String>,
    pub created_at: i64,
    pub last_used:  i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct NostrCacheEntry {
    pub event_id:   String,
    pub kind:       i64,
    pub pubkey:     String,
    pub d_tag:      Option<String>,
    pub t_tags:     String,
    pub content:    String,
    pub tags_json:  String,
    pub created_at: i64,
    pub indexed_at: i64,
}