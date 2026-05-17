use axum::{extract::{State, Path, Query}, Json};
use serde::{Deserialize, Serialize};
use crate::{AppState, auth::AuthUser, error::{AppError, AppResult}};

#[derive(Deserialize)]
pub struct SubmitRatingInput {
    pub ride_id: String,
    pub driver_pubkey: String,
    pub score: i64,
    pub comment: Option<String>,
    pub category: Option<String>,
}

#[derive(Serialize)]
pub struct RatingResponse {
    pub id: String,
    pub new_avg: f64,
    pub total_ratings: i64,
}

pub async fn submit_rating(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<SubmitRatingInput>,
) -> AppResult<Json<RatingResponse>> {
    if body.score < 1 || body.score > 5 {
        return Err(AppError::BadRequest("Score must be 1-5".into()));
    }
    let already: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM ratings WHERE ride_id=?1 AND rider_pubkey=?2)"
    ).bind(&body.ride_id).bind(&auth.public_key)
    .fetch_one(&state.db).await.unwrap_or(false);
    if already { return Err(AppError::BadRequest("Already rated".into())); }

    let id = uuid::Uuid::new_v4().to_string().replace('-', "");
    let now = chrono::Utc::now().timestamp();
    let cat = body.category.as_deref().unwrap_or("city");

    sqlx::query("INSERT INTO ratings (id,ride_id,driver_pubkey,rider_pubkey,score,comment,category,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")
        .bind(&id).bind(&body.ride_id).bind(&body.driver_pubkey).bind(&auth.public_key)
        .bind(body.score).bind(body.comment.as_deref().unwrap_or("")).bind(cat).bind(now)
        .execute(&state.db).await.map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;

    sqlx::query(
        "INSERT INTO driver_stats (pubkey,total_rides,total_ratings,sum_scores,avg_rating,category,updated_at) VALUES (?1,1,1,?2,?3,?4,?5)
         ON CONFLICT(pubkey) DO UPDATE SET total_ratings=driver_stats.total_ratings+1, sum_scores=driver_stats.sum_scores+?2,
         avg_rating=CAST((driver_stats.sum_scores+?2) AS REAL)/(driver_stats.total_ratings+1), updated_at=?5"
    ).bind(&body.driver_pubkey).bind(body.score).bind(body.score as f64).bind(cat).bind(now)
    .execute(&state.db).await.map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;

    let (new_avg, total): (f64, i64) = sqlx::query_as(
        "SELECT avg_rating, total_ratings FROM driver_stats WHERE pubkey=?1"
    ).bind(&body.driver_pubkey).fetch_one(&state.db).await.unwrap_or((0.0, 0));

    Ok(Json(RatingResponse { id, new_avg, total_ratings: total }))
}

#[derive(Serialize)]
pub struct DriverRating {
    pub pubkey: String,
    pub avg_rating: f64,
    pub total_ratings: i64,
    pub total_rides: i64,
}

pub async fn get_driver_rating(
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
) -> AppResult<Json<DriverRating>> {
    let stats: Option<(f64, i64, i64)> = sqlx::query_as(
        "SELECT avg_rating, total_ratings, total_rides FROM driver_stats WHERE pubkey=?1"
    ).bind(&pubkey).fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;
    let (avg, ratings, rides) = stats.unwrap_or((0.0, 0, 0));
    Ok(Json(DriverRating { pubkey, avg_rating: avg, total_ratings: ratings, total_rides: rides }))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct RatingDetail {
    pub id: String,
    pub score: i64,
    pub comment: String,
    pub rider_pubkey: String,
    pub created_at: i64,
}

pub async fn get_driver_ratings(
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
) -> AppResult<Json<Vec<RatingDetail>>> {
    let ratings = sqlx::query_as::<_, RatingDetail>(
        "SELECT id, score, comment, rider_pubkey, created_at FROM ratings WHERE driver_pubkey=?1 ORDER BY created_at DESC LIMIT 50"
    ).bind(&pubkey).fetch_all(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;
    Ok(Json(ratings))
}

#[derive(Deserialize)]
pub struct LeaderboardQuery {
    pub category: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct LeaderboardEntry {
    pub pubkey: String,
    pub avg_rating: f64,
    pub total_ratings: i64,
    pub total_rides: i64,
    pub category: String,
}

pub async fn leaderboard(
    State(state): State<AppState>,
    Query(q): Query<LeaderboardQuery>,
) -> AppResult<Json<Vec<LeaderboardEntry>>> {
    let cat = q.category.as_deref().unwrap_or("");
    let lim = q.limit.unwrap_or(20);
    let entries = sqlx::query_as::<_, LeaderboardEntry>(
        "SELECT pubkey, avg_rating, total_ratings, total_rides, category FROM driver_stats
         WHERE total_ratings >= 1 AND (?1='' OR category=?1) ORDER BY avg_rating DESC, total_ratings DESC LIMIT ?2"
    ).bind(cat).bind(lim).fetch_all(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;
    Ok(Json(entries))
}

#[derive(Serialize)]
pub struct DashboardData {
    pub total_rides: i64,
    pub total_ratings: i64,
    pub avg_rating: f64,
    pub total_earned_sats: i64,
    pub total_earned_mwk: i64,
    pub recent_rides: Vec<RecentRide>,
    pub listings: Vec<ListingStats>,
    pub recent_reviews: Vec<RatingDetail>,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct ListingStats {
    pub id:                  String,
    pub listing_name:        Option<String>,
    pub vehicle:             Option<String>,
    pub vehicle_type:        Option<String>,
    pub photo_urls:          Option<String>,
    pub price_per_km:        Option<i64>,
    pub service_interval_km: i64,
    pub km_driven_total:     i64,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct RecentRide {
    pub id: String,
    pub rider_pubkey: String,
    pub pickup_lat: f64,
    pub pickup_lng: f64,
    pub fare_sats: i64,
    pub status: String,
    pub created_at: i64,
}

pub async fn driver_dashboard(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<DashboardData>> {
    let stats: Option<(i64, i64, f64, i64, i64)> = sqlx::query_as(
        "SELECT total_rides, total_ratings, avg_rating, total_earned_sats, total_earned_mwk FROM driver_stats WHERE pubkey=?1"
    ).bind(&auth.public_key).fetch_optional(&state.db).await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))?;
    let (rides, ratings, avg, sats, mwk) = stats.unwrap_or((0, 0, 0.0, 0, 0));

    let recent = sqlx::query_as::<_, RecentRide>(
        "SELECT id, rider_pubkey, pickup_lat, pickup_lng, fare_sats, status, created_at
         FROM ride_requests WHERE matched_driver=?1 ORDER BY created_at DESC LIMIT 10"
    ).bind(&auth.public_key).fetch_all(&state.db).await.unwrap_or_default();

    // Active listings with per-listing km tracking + service interval.
    // Frontend computes service-due as: ceil(km_driven_total / service_interval_km) * service_interval_km.
    let listings = sqlx::query_as::<_, ListingStats>(
        "SELECT id, listing_name, vehicle, vehicle_type, photo_urls, price_per_km,
                service_interval_km, km_driven_total
         FROM driver_listings
         WHERE driver_pubkey = ?1 AND deleted_at IS NULL
         ORDER BY updated_at DESC"
    ).bind(&auth.public_key).fetch_all(&state.db).await.unwrap_or_default();

    // Recent reviews — same shape as /ratings/reviews/:pubkey, embedded for one round-trip.
    let recent_reviews = sqlx::query_as::<_, RatingDetail>(
        "SELECT id, score, comment, rider_pubkey, created_at
         FROM ratings WHERE driver_pubkey = ?1
         ORDER BY created_at DESC LIMIT 20"
    ).bind(&auth.public_key).fetch_all(&state.db).await.unwrap_or_default();

    Ok(Json(DashboardData {
        total_rides: rides,
        total_ratings: ratings,
        avg_rating: avg,
        total_earned_sats: sats,
        total_earned_mwk: mwk,
        recent_rides: recent,
        listings,
        recent_reviews,
    }))
}
