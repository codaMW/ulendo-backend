use nostr_sdk::{Client, Filter, Kind, RelayPoolNotification};
use sqlx::SqlitePool;
use std::time::Duration;
use tokio::time::sleep;

pub async fn run_indexer(pool: SqlitePool, relay_urls: Vec<String>) {
    tracing::info!("nostr indexer started, relays: {:?}", relay_urls);
    loop {
        if let Err(e) = index_once(&pool, &relay_urls).await {
            tracing::warn!("nostr indexer error: {e}, retrying in 30s");
            sleep(Duration::from_secs(30)).await;
        }
    }
}

async fn index_once(pool: &SqlitePool, relay_urls: &[String]) -> anyhow::Result<()> {
    let client = Client::default();

    for url in relay_urls {
        client.add_relay(url).await?;
    }
    client.connect().await;

    let filter = Filter::new()
        .kind(Kind::Custom(30402))
        .hashtag("ulendo");

    client.subscribe(vec![filter], None).await?;

    let mut notifications = client.notifications();

    loop {
        match notifications.recv().await {
            Ok(RelayPoolNotification::Event { event, .. }) => {
                let event_id   = event.id.to_hex();
                let pubkey     = event.pubkey.to_hex();
                let kind       = event.kind.as_u64() as i64;
                let created_at = event.created_at.as_u64() as i64;
                let content    = event.content.clone();
                let now        = chrono::Utc::now().timestamp();

                // nostr-sdk 0.34: Tag implements Serialize directly
                let tags_json = serde_json::to_string(event.tags.as_slice())
                    .unwrap_or_else(|_| "[]".into());

                // Extract d tag — Tag::as_vec() returns &[String]
                let d_tag: Option<String> = event.tags.iter()
                    .find(|t| t.as_vec().first().map(String::as_str) == Some("d"))
                    .and_then(|t| t.as_vec().get(1).cloned());

                // Extract all t tag values
                let t_tags: Vec<String> = event.tags.iter()
                    .filter(|t| t.as_vec().first().map(String::as_str) == Some("t"))
                    .filter_map(|t| t.as_vec().get(1).cloned())
                    .collect();
                let t_tags_json = serde_json::to_string(&t_tags).unwrap_or_else(|_| "[]".into());

                let res = sqlx::query(
                    r#"INSERT INTO nostr_relay_cache
                       (event_id, kind, pubkey, d_tag, t_tags, content, tags_json, created_at, indexed_at)
                       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                       ON CONFLICT(event_id) DO UPDATE SET
                           content    = excluded.content,
                           tags_json  = excluded.tags_json,
                           indexed_at = excluded.indexed_at"#
                )
                .bind(&event_id)
                .bind(kind)
                .bind(&pubkey)
                .bind(&d_tag)
                .bind(&t_tags_json)
                .bind(&content)
                .bind(&tags_json)
                .bind(created_at)
                .bind(now)
                .execute(pool)
                .await;

                if let Err(e) = res {
                    tracing::warn!("failed to cache nostr event {event_id}: {e}");
                } else {
                    tracing::debug!("indexed nostr event {event_id} kind:{kind}");
                }
            }
            Ok(RelayPoolNotification::RelayStatus { .. }) => {}
            Err(_) => {
                tracing::warn!("nostr notification channel closed, reconnecting");
                break;
            }
            _ => {}
        }
    }

    client.disconnect().await?;
    Ok(())
}