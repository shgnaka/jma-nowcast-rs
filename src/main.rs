//! CLI entry point for jma-nowcast-rs.

mod api;
mod api_policy;
mod mcp;
mod mcp_http;

use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand};
use reqwest::Client;
use std::net::SocketAddr;
use std::sync::Arc;

use jma_nowcast_rs::client::DEFAULT_JMA_BASE_URL;
use jma_nowcast_rs::query::{Nowcast, Waypoint};
use jma_nowcast_rs::short_range::{get_short_range, DEFAULT_SHORT_RANGE_BASE_URL};

#[derive(Parser, Debug)]
#[command(
    name = "jma-nowcast-rs",
    version,
    about = "JMA high-resolution precipitation nowcast CLI, REST, and MCP server"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Get current precipitation observation at a point
    Precipitation(PointArgs),
    /// Get precipitation nowcast forecast samples at a point
    Nowcast(PointArgs),
    /// Get area precipitation histogram/counts in a bounding box
    AreaNowcast(AreaArgs),
    /// Find nearest rain within radius
    NearestRain(NearestArgs),
    /// Evaluate precipitation along a route
    RouteNowcast(RouteArgs),
    /// Find rain-free time windows within forecast horizon
    RainFreeWindows(PointArgs),
    /// Get hourly short-range precipitation forecast (R10)
    ShortRange(PointArgs),
    /// Run REST API HTTP server
    Serve(ServeArgs),
    /// Run stdio Model Context Protocol (MCP) server
    Mcp,
}

#[derive(Args, Debug)]
struct PointArgs {
    #[arg(long)]
    lat: f64,
    #[arg(long)]
    lon: f64,
    #[arg(long, default_value_t = 10)]
    zoom: u8,
}

#[derive(Args, Debug)]
struct AreaArgs {
    #[arg(long)]
    south: f64,
    #[arg(long)]
    west: f64,
    #[arg(long)]
    north: f64,
    #[arg(long)]
    east: f64,
    #[arg(long, default_value_t = 10)]
    zoom: u8,
    #[arg(long, default_value_t = 0)]
    lead_minutes: u8,
}

#[derive(Args, Debug)]
struct NearestArgs {
    #[arg(long)]
    lat: f64,
    #[arg(long)]
    lon: f64,
    #[arg(long)]
    radius_km: f64,
    #[arg(long, default_value_t = 10)]
    zoom: u8,
    #[arg(long, default_value_t = 0)]
    lead_minutes: u8,
}

#[derive(Args, Debug)]
struct RouteArgs {
    /// JSON array of waypoints or file path containing JSON array of waypoints
    #[arg(long)]
    points: String,
    #[arg(long, default_value_t = 10)]
    zoom: u8,
}

#[derive(Args, Debug)]
struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: String,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let production = std::env::var_os("JMA_PRODUCTION").is_some();
    if production && std::env::var_os("JMA_FIXED_NOW").is_some() {
        return Err("JMA_FIXED_NOW is forbidden in production".into());
    }

    let base_url =
        std::env::var("JMA_BASE_URL").unwrap_or_else(|_| DEFAULT_JMA_BASE_URL.to_string());
    let fixed_now = match std::env::var("JMA_FIXED_NOW") {
        Ok(s) => match DateTime::parse_from_rfc3339(&s) {
            Ok(dt) => Some(dt.with_timezone(&Utc)),
            Err(_) => {
                let err_json = serde_json::json!({
                    "error": format!("Invalid JMA_FIXED_NOW RFC3339 timestamp: {s}"),
                    "code": "invalid_input"
                });
                return Err(err_json.to_string().into());
            }
        },
        Err(_) => None,
    };

    let short_range_base_url = std::env::var("JMA_SHORT_RANGE_BASE_URL")
        .unwrap_or_else(|_| DEFAULT_SHORT_RANGE_BASE_URL.to_string());

    let http_client = Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let nowcast = Arc::new(Nowcast::new(&base_url, fixed_now)?);

    match cli.command {
        Commands::Precipitation(args) => {
            let res = nowcast
                .get_precipitation(args.lat, args.lon, args.zoom)
                .await
                .map_err(|e| {
                    serde_json::json!({
                        "error": e.to_string(),
                        "code": e.code()
                    })
                    .to_string()
                })?;
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        Commands::Nowcast(args) => {
            let res = nowcast
                .get_nowcast(args.lat, args.lon, args.zoom)
                .await
                .map_err(|e| {
                    serde_json::json!({
                        "error": e.to_string(),
                        "code": e.code()
                    })
                    .to_string()
                })?;
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        Commands::AreaNowcast(args) => {
            let res = nowcast
                .get_area_nowcast(
                    args.south,
                    args.west,
                    args.north,
                    args.east,
                    args.zoom,
                    args.lead_minutes,
                )
                .await
                .map_err(|e| {
                    serde_json::json!({
                        "error": e.to_string(),
                        "code": e.code()
                    })
                    .to_string()
                })?;
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        Commands::NearestRain(args) => {
            let res = nowcast
                .get_nearest_rain(
                    args.lat,
                    args.lon,
                    args.radius_km,
                    args.zoom,
                    args.lead_minutes,
                )
                .await
                .map_err(|e| {
                    serde_json::json!({
                        "error": e.to_string(),
                        "code": e.code()
                    })
                    .to_string()
                })?;
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        Commands::RouteNowcast(args) => {
            let points_json = if args.points.trim_start().starts_with('[') {
                args.points
            } else {
                std::fs::read_to_string(&args.points)?
            };
            let waypoints: Vec<Waypoint> = serde_json::from_str(&points_json)?;
            let res = nowcast
                .get_route_nowcast(waypoints, args.zoom)
                .await
                .map_err(|e| {
                    serde_json::json!({
                        "error": e.to_string(),
                        "code": e.code()
                    })
                    .to_string()
                })?;
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        Commands::RainFreeWindows(args) => {
            let res = nowcast
                .get_rain_free_windows(args.lat, args.lon, args.zoom)
                .await
                .map_err(|e| {
                    serde_json::json!({
                        "error": e.to_string(),
                        "code": e.code()
                    })
                    .to_string()
                })?;
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        Commands::ShortRange(args) => {
            let now = nowcast.now();
            let res = get_short_range(
                &http_client,
                &short_range_base_url,
                now,
                args.lat,
                args.lon,
                args.zoom,
            )
            .await
            .map_err(|e| {
                serde_json::json!({
                    "error": e.to_string(),
                    "code": e.code()
                })
                .to_string()
            })?;
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        Commands::Serve(args) => {
            let policy = api_policy::ApiPolicy::from_env(production)?;
            let mcp_policy = api_policy::ApiPolicy::mcp_from_env(&policy)?;
            let mcp_state = api::AppState {
                nowcast: nowcast.clone(),
                http_client: http_client.clone(),
                short_range_base_url: short_range_base_url.clone(),
            };
            let mut router = api::app(nowcast, http_client, short_range_base_url, policy);
            if let Some(policy) = mcp_policy {
                router = router.merge(mcp_http::router(mcp_state, policy));
            }
            let addr: SocketAddr = args.bind.parse()?;
            println!("Listening on http://{}", addr);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            axum::serve(listener, router).await?;
        }
        Commands::Mcp => {
            mcp::run_mcp_server(nowcast, http_client, short_range_base_url).await?;
        }
    }

    Ok(())
}
