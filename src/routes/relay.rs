use axum::{extract::{Query, State}, Json};
use serde::{Deserialize, Serialize};
use crate::{db::NostrCacheEntry, error::AppResult, AppState};

/// GET /relay/listings?category=guide&area=Lilongwe&limit=50
///
/// Serves searches over the backend's local Nostr event index.
/// Much faster than querying relays directly, and works offline.
/// The indexer (services/nostr.rs) keeps this fresh in the background.

#[derive(Deserialize)]
pub struct RelaySearchQuery {
    pub category: Option<String>,
    pub area:     Option<String>,
    pub author:   Option<String>,   // hex pubkey — get one merchant's listings
    pub limit:    Option<i64>,
    pub offset:   Option<i64>,
}

#[derive(Serialize)]
pub struct RelaySearchResponse {
    pub items:  Vec<NostrCacheEntry>,
    pub total:  i64,
    pub limit:  i64,
    pub offset: i64,
}

pub async fn search_listings(
    Query(q): Query<RelaySearchQuery>,
    State(state): State<AppState>,
) -> AppResult<Json<RelaySearchResponse>> {
    let limit  = q.limit.unwrap_or(50).min(200);
    let offset = q.offset.unwrap_or(0);

    // We filter on t_tags (JSON array) using SQLite's json_each for the category,
    // and on the content for area (free text match).
    // In production you'd add FTS5 for proper full-text search.

    let items = sqlx::query_as::<_, NostrCacheEntry>(
        r#"SELECT DISTINCT c.*
           FROM nostr_relay_cache c
           WHERE c.kind = 30402
             AND (?1 IS NULL OR EXISTS (
                 SELECT 1 FROM json_each(c.tags_json) jt
                 WHERE json_extract(jt.value, '$[0]') = 'c'
                   AND json_extract(jt.value, '$[1]') = ?1
             ))
             AND (?2 IS NULL OR EXISTS (
                 SELECT 1 FROM json_each(c.tags_json) jt
                 WHERE json_extract(jt.value, '$[0]') = 'location'
                   AND json_extract(jt.value, '$[1]') LIKE '%' || ?2 || '%'
             ))
             AND (?3 IS NULL OR c.pubkey = ?3)
           ORDER BY c.created_at DESC
           LIMIT ?4 OFFSET ?5"#
    )
    .bind(&q.category)
    .bind(&q.area)
    .bind(&q.author)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    let total: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(DISTINCT c.event_id)
           FROM nostr_relay_cache c
           WHERE c.kind = 30402
             AND (?1 IS NULL OR EXISTS (
                 SELECT 1 FROM json_each(c.tags_json) jt
                 WHERE json_extract(jt.value, '$[0]') = 'c'
                   AND json_extract(jt.value, '$[1]') = ?1
             ))
             AND (?2 IS NULL OR EXISTS (
                 SELECT 1 FROM json_each(c.tags_json) jt
                 WHERE json_extract(jt.value, '$[0]') = 'location'
                   AND json_extract(jt.value, '$[1]') LIKE '%' || ?2 || '%'
             ))
             AND (?3 IS NULL OR c.pubkey = ?3)"#
    )
    .bind(&q.category)
    .bind(&q.area)
    .bind(&q.author)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(RelaySearchResponse { items, total, limit, offset }))
}