//! REST API endpoints for JMA Nowcast.

use axum::{
    extract::{rejection::JsonRejection, FromRequest, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use jma_nowcast_rs::query::{Nowcast, NowcastError, Waypoint};
use jma_nowcast_rs::short_range::{get_short_range, ShortRange, ShortRangeError};
use jma_nowcast_rs::tiles::DEFAULT_ZOOM;

#[derive(Clone)]
pub struct AppState {
    pub nowcast: Arc<Nowcast>,
    pub http_client: Client,
    pub short_range_base_url: String,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct PointRequest {
    pub lat: f64,
    pub lon: f64,
    #[serde(default = "default_zoom")]
    pub zoom: u8,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct AreaRequest {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
    #[serde(default = "default_zoom")]
    pub zoom: u8,
    #[serde(default)]
    pub lead_minutes: u8,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct NearestRequest {
    pub lat: f64,
    pub lon: f64,
    pub radius_km: f64,
    #[serde(default = "default_zoom")]
    pub zoom: u8,
    #[serde(default)]
    pub lead_minutes: u8,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct RouteRequest {
    pub points: Vec<Waypoint>,
    #[serde(default = "default_zoom")]
    pub zoom: u8,
}

fn default_zoom() -> u8 {
    DEFAULT_ZOOM
}

/// Custom Json extractor that turns extraction rejections into structured 400 responses.
pub struct AppJson<T>(pub T);

impl<S, T> FromRequest<S> for AppJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(AppJson(value)),
            Err(rejection) => {
                let body = Json(json!({
                    "error": rejection.body_text(),
                    "code": "invalid_input",
                }));
                Err((StatusCode::BAD_REQUEST, body).into_response())
            }
        }
    }
}

pub struct ApiError(pub String, pub &'static str, pub StatusCode);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(json!({
            "error": self.0,
            "code": self.1,
        }));
        (self.2, body).into_response()
    }
}

impl From<NowcastError> for ApiError {
    fn from(err: NowcastError) -> Self {
        let code = err.code();
        let status = if code == "invalid_input" {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::BAD_GATEWAY
        };
        ApiError(err.to_string(), code, status)
    }
}

impl From<ShortRangeError> for ApiError {
    fn from(err: ShortRangeError) -> Self {
        let code = err.code();
        let status = if code == "invalid_input" {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::BAD_GATEWAY
        };
        ApiError(err.to_string(), code, status)
    }
}

pub fn app(nowcast: Arc<Nowcast>, http_client: Client, short_range_base_url: String) -> Router {
    Router::new()
        .route("/v1/precipitation", post(precipitation_handler))
        .route("/v1/nowcast", post(nowcast_handler))
        .route("/v1/area-nowcast", post(area_nowcast_handler))
        .route("/v1/nearest-rain", post(nearest_rain_handler))
        .route("/v1/route-nowcast", post(route_nowcast_handler))
        .route("/v1/rain-free-windows", post(rain_free_windows_handler))
        .route("/v1/short-range", post(short_range_handler))
        .fallback(not_found_handler)
        .with_state(AppState {
            nowcast,
            http_client,
            short_range_base_url,
        })
}

async fn not_found_handler() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "route not found",
            "code": "not_found",
        })),
    )
        .into_response()
}

async fn precipitation_handler(
    State(state): State<AppState>,
    AppJson(payload): AppJson<PointRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let res = state
        .nowcast
        .get_precipitation(payload.lat, payload.lon, payload.zoom)
        .await?;
    Ok(Json(res))
}

async fn nowcast_handler(
    State(state): State<AppState>,
    AppJson(payload): AppJson<PointRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let res = state
        .nowcast
        .get_nowcast(payload.lat, payload.lon, payload.zoom)
        .await?;
    Ok(Json(res))
}

async fn area_nowcast_handler(
    State(state): State<AppState>,
    AppJson(payload): AppJson<AreaRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let res = state
        .nowcast
        .get_area_nowcast(
            payload.south,
            payload.west,
            payload.north,
            payload.east,
            payload.zoom,
            payload.lead_minutes,
        )
        .await?;
    Ok(Json(res))
}

async fn nearest_rain_handler(
    State(state): State<AppState>,
    AppJson(payload): AppJson<NearestRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let res = state
        .nowcast
        .get_nearest_rain(
            payload.lat,
            payload.lon,
            payload.radius_km,
            payload.zoom,
            payload.lead_minutes,
        )
        .await?;
    Ok(Json(res))
}

async fn route_nowcast_handler(
    State(state): State<AppState>,
    AppJson(payload): AppJson<RouteRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let res = state
        .nowcast
        .get_route_nowcast(payload.points, payload.zoom)
        .await?;
    Ok(Json(res))
}

async fn rain_free_windows_handler(
    State(state): State<AppState>,
    AppJson(payload): AppJson<PointRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let res = state
        .nowcast
        .get_rain_free_windows(payload.lat, payload.lon, payload.zoom)
        .await?;
    Ok(Json(res))
}

async fn short_range_handler(
    State(state): State<AppState>,
    AppJson(payload): AppJson<PointRequest>,
) -> Result<Json<ShortRange>, ApiError> {
    let now = state.nowcast.now();
    let res = get_short_range(
        &state.http_client,
        &state.short_range_base_url,
        now,
        payload.lat,
        payload.lon,
        payload.zoom,
    )
    .await?;
    Ok(Json(res))
}
