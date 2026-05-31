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
    pub min_fare_sats:     i64,
    pub min_price_per_km_sats: i64,
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
                                   .unwrap_or_else(|_| "150".into()).parse().unwrap_or(1000),
            min_fare_sats:     std::env::var("MIN_FARE_SATS")
                                   .unwrap_or_else(|_| "200".into()).parse().unwrap_or(200),
            min_price_per_km_sats: std::env::var("MIN_PRICE_PER_KM_SATS")
                                   .unwrap_or_else(|_| "50".into()).parse().unwrap_or(50),
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
    tracing::info!("Starting Ulendo backend v1.1 on {}:{}", cfg.host, cfg.port);

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

    // Background: auto-release completed bookings after 60 seconds
    let release_state = state.clone();
    tokio::spawn(async move {
        services::blink::run_auto_release(release_state).await;
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
        .route("/.well-known/nostr.json", get(routes::names::well_known_nostr_json))
        .route("/names/check", get(routes::names::check_username))
        .route("/names/register", post(routes::names::register_name))
        .route("/names/by-pubkey/:pubkey", get(routes::names::get_name_by_pubkey))
                .route("/chessa/quote", post(routes::chessa::create_quote))
        .route("/chessa/pay", post(routes::chessa::pay_order))
        .route("/chessa/order-status/:id", get(routes::chessa::get_order_status))
        .route("/chessa/config", get(routes::chessa::get_config))
        .route("/chessa/pay-lightning", post(routes::chessa::pay_lightning))
        .route("/health", get(health))
        .route("/version", get(|| async { "ulendo-backend-v2-presence" }))
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
        .route("/escrow/:id/rider-confirm",  post(routes::confirm::rider_confirm))
        .route("/escrow/:id/driver-confirm", post(routes::confirm::driver_confirm))
        .route("/escrow/:id/pickup",         post(routes::confirm::confirm_pickup))
        .route("/escrow/:id/cancel",         post(routes::confirm::cancel_before_pickup))
        .route("/escrow/:id/dispute",     post(routes::escrow::dispute))
        .route("/escrow/release-direct",  post(routes::escrow::release_direct))
        .route("/listings/driver",                post(routes::driver_listings::upsert))
        .route("/listings/driver/mine",           get(routes::driver_listings::list_mine))
        .route("/listings/driver/:id",            delete(routes::driver_listings::delete_one))
        .route("/listings/driver/:id/km",         post(routes::driver_listings::add_km))
        .route("/rides/nearby-listings",          get(routes::driver_listings::nearby_listings))
        .route("/escrow/direct-release-status/:ride_id", axum::routing::get(routes::escrow::direct_release_status))
        // Rides — driver discovery
        .route("/rides/request",          post(routes::rides::request_ride))
        .route("/rides/accept",           post(routes::rides::accept_ride))
        .route("/rides/nearby",           post(routes::rides::nearby_drivers))
        .route("/rides/test-driver",      post(routes::rides::test_add_driver))
        .route("/rides/heartbeat",        post(routes::rides::http_heartbeat))
        .route("/rides/notify",           post(routes::rides::http_send_booking))
        .route("/rides/poll-bookings",    get(routes::rides::poll_bookings))
        .route("/rides/accept-booking",   post(routes::rides::accept_booking_http))
        .route("/rides/status/:ride_id",   get(routes::rides::poll_ride_status))
        .route("/ratings",                post(routes::ratings::submit_rating))
        .route("/ratings/driver/:pubkey", get(routes::ratings::get_driver_rating))
        .route("/ratings/reviews/:pubkey", get(routes::ratings::get_driver_ratings))
        .route("/ratings/leaderboard",    get(routes::ratings::leaderboard))
        // ── FIAT ROUTES DISABLED ──────────────────────────────────────────────
        // Fiat money flow has been removed pending PayChangu API integration.
        // Previous routes had several critical security issues (driver could
        // self-release escrow, pending_payouts had no auth and leaked phone
        // numbers, errors silently swallowed). When PayChangu integration is
        // ready, these will be replaced with a properly designed payment flow.
        // .route("/fiat/escrow",post(routes::fiat::create_fiat_escrow))
        // .route("/fiat/verify-sms",post(routes::fiat::verify_sms))
        // .route("/fiat/release",post(routes::fiat::release_fiat))
        // .route("/fiat/payouts",get(routes::fiat::pending_payouts))
        .route("/ratings/dashboard",      get(routes::ratings::driver_dashboard))
        .route("/escrow/:id/refund",      post(routes::escrow::refund))
        .route("/escrow/:id/complete",    post(routes::escrow::complete))
        // Push
        .route("/push/vapid-key",         get(routes::push::vapid_public_key))
        .route("/push/subscribe",         post(routes::push::subscribe))
        .route("/push/unsubscribe",       delete(routes::push::unsubscribe))
        .route("/ws", get(routes::ws::ws_handler))
        .route("/upload/photo", post(routes::upload::upload_photo))
        // Relay cache
        .route("/relay/listings",         get(routes::relay::search_listings))
        .route("/verify/invoice",       post(routes::upload::create_verify_invoice))
        .route("/verify/invoice/check", post(routes::upload::check_verify_invoice))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(10 * 1024 * 1024))
        .with_state(state);

    let addr = format!("{}:{}", cfg.host, cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Listening on http://{}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}
// force deploy 1775824761
// force deploy 1775824797
