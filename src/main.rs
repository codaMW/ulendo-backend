use anyhow::Result;
use axum::{
    routing::{delete, get, patch, post},
    Router,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::{str::FromStr, sync::Arc, time::Duration};
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub mod auth;
pub mod db;
pub mod error;
pub mod routes;
pub mod services;

pub use error::{AppError, AppResult};
pub use routes::ws::WsRegistry;


// ── Shared state ──────────────────────────────────────────────────────────
#[derive(Clone)]
pub struct AppState {
    pub db:    sqlx::SqlitePool,
    pub cfg:   Arc<Config>,
    pub blink: Arc<services::blink::BlinkClient>,
    pub push:  Arc<services::push::PushService>,
    pub ws:    WsRegistry,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub host:              String,
    pub port:              u16,
    pub database_url:      String,
    pub blink_api_key:     String,
    pub blink_wallet_id:   String,
    pub blink_api_url:     String,
    pub nostr_relays:      Vec<String>,
    pub vapid_subject:     String,
    pub vapid_public_key:  String,
    pub vapid_private_key: String,
    pub frontend_origin:   String,
    pub rate_limit_rpm:    u32,
    pub escrow_fee_bps:    u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        Ok(Config {
            host:              std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port:              std::env::var("PORT").unwrap_or_else(|_| "8080".into()).parse()?,
            database_url:      std::env::var("DATABASE_URL")
                                   .unwrap_or_else(|_| "sqlite:./ulendo.db".into()),
            blink_api_key:     std::env::var("BLINK_API_KEY").unwrap_or_default(),
            blink_wallet_id:   std::env::var("BLINK_WALLET_ID").unwrap_or_default(),
            blink_api_url:     std::env::var("BLINK_API_URL")
                                   .unwrap_or_else(|_| "https://api.blink.sv/graphql".into()),
            nostr_relays:      std::env::var("NOSTR_RELAYS")
                                   .unwrap_or_else(|_| "wss://relay.damus.io".into())
                                   .split(',').map(|s| s.trim().to_string()).collect(),
            vapid_subject:     std::env::var("VAPID_SUBJECT").unwrap_or_default(),
            vapid_public_key:  std::env::var("VAPID_PUBLIC_KEY").unwrap_or_default(),
            vapid_private_key: std::env::var("VAPID_PRIVATE_KEY").unwrap_or_default(),
            frontend_origin:   std::env::var("FRONTEND_ORIGIN")
                                   .unwrap_or_else(|_| "http://localhost:5173".into()),
            rate_limit_rpm:    std::env::var("RATE_LIMIT_RPM")
                                   .unwrap_or_else(|_| "60".into()).parse().unwrap_or(60),
            escrow_fee_bps:    std::env::var("ESCROW_FEE_BPS")
                                   .unwrap_or_else(|_| "150".into()).parse().unwrap_or(150),
        })
    }
}

// ── Health ────────────────────────────────────────────────────────────────────
async fn health(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    let ok = sqlx::query("SELECT 1").fetch_one(&state.db).await.is_ok();
    axum::Json(serde_json::json!({ "ok": ok, "service": "ulendo-backend" }))
}

// ── Main ──────────────────────────────────────────────────────────────────────
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "ulendo=debug,tower_http=info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = Config::from_env()?;
    tracing::info!("Starting Ulendo backend on {}:{}", cfg.host, cfg.port);

    let opts = SqliteConnectOptions::from_str(&cfg.database_url)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(opts)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;
    tracing::info!("Migrations applied");

    let blink = Arc::new(services::blink::BlinkClient::new(
        cfg.blink_api_url.clone(),
        cfg.blink_api_key.clone(),
        cfg.blink_wallet_id.clone(),
    ));

    let push = Arc::new(services::push::PushService::new(
        cfg.vapid_subject.clone(),
        cfg.vapid_public_key.clone(),
        cfg.vapid_private_key.clone(),
    ));

    let state = AppState {
        db:    pool.clone(),
        cfg:   Arc::new(cfg.clone()),
        blink,
        push,
        ws: routes::ws::new_registry(),
    };

    // Background: Nostr relay indexer
    let idx_pool   = pool.clone();
    let idx_relays = cfg.nostr_relays.clone();
    tokio::spawn(async move {
        services::nostr::run_indexer(idx_pool, idx_relays).await;
    });

    // Background: escrow payment monitor
    let escrow_state = state.clone();
    tokio::spawn(async move {
        services::blink::run_escrow_monitor(escrow_state).await;
    });

    let origins: Vec<axum::http::HeaderValue> = vec![
        cfg.frontend_origin.parse()?,
        "http://localhost:5173".parse()?,
        "http://localhost:4173".parse()?,
        "https://ulendo-malawi.vercel.app".parse()?,
    ];
    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods(Any)
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            axum::http::header::ACCEPT,
        ]);

    let app = Router::new()
        .route("/health", get(health))
        // Identities
        .route("/identities",        post(routes::identities::upsert))
        .route("/identities/:npub",  get(routes::identities::get_by_npub))
        // Listings
        .route("/listings",          get(routes::listings::list))
        .route("/listings",          post(routes::listings::create))
        .route("/listings/:id",      get(routes::listings::get_one))
        .route("/listings/:id",      patch(routes::listings::update))
        .route("/listings/:id",      delete(routes::listings::remove))
        // Bookings
        .route("/bookings",               post(routes::bookings::create))
        .route("/bookings/:id",           get(routes::bookings::get_one))
        .route("/bookings/:id/status",    patch(routes::bookings::update_status))
        // Escrow
        .route("/escrow/:id/fund",        post(routes::escrow::fund))
        .route("/escrow/:id/release",     post(routes::escrow::release))
        .route("/escrow/:id/dispute",     post(routes::escrow::dispute))
        .route("/escrow/:id/refund",      post(routes::escrow::refund))
        // Push
        .route("/push/vapid-key",         get(routes::push::vapid_public_key))
        .route("/push/subscribe",         post(routes::push::subscribe))
        .route("/push/unsubscribe",       delete(routes::push::unsubscribe))
        .route("/ws", get(routes::ws::ws_handler))
        .route("/upload/photo", post(routes::upload::upload_photo))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
        )
        // Relay cache
        .route("/relay/listings",         get(routes::relay::search_listings))
        .route("/verify/invoice",       post(routes::upload::create_verify_invoice))
        .route("/verify/invoice/check", post(routes::upload::check_verify_invoice))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(256 * 1024))
        .with_state(state);

    let addr = format!("{}:{}", cfg.host, cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Listening on http://{}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}
