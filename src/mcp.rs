//! Model Context Protocol (MCP) stdio server implementation.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::api::{AreaRequest, NearestRequest, PointRequest, RouteRequest};
use jma_nowcast_rs::query::Nowcast;
use jma_nowcast_rs::short_range::get_short_range;

pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2024-11-05"];
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Deserialize, Debug)]
struct RawJsonRpcRequest {
    jsonrpc: Option<Value>,
    id: Option<Value>,
    method: Option<Value>,
    params: Option<Value>,
}

#[derive(Serialize, Debug)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize, Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

pub async fn run_mcp_server(
    nowcast: Arc<Nowcast>,
    http_client: Client,
    short_range_base_url: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let raw: RawJsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {
                        "code": -32700,
                        "message": format!("Parse error: {}", e),
                    }
                });
                let mut out = serde_json::to_string(&err_resp)?;
                out.push('\n');
                stdout.write_all(out.as_bytes()).await?;
                stdout.flush().await?;
                continue;
            }
        };

        // If it's a notification (id is None), do NOT send any response per JSON-RPC 2.0 and MCP spec.
        let req_id = match raw.id {
            Some(id) if !id.is_null() => id,
            _ => {
                // No id -> notification. Ignore without response.
                continue;
            }
        };

        // Validate jsonrpc version == "2.0"
        let jsonrpc_valid = match raw.jsonrpc {
            Some(Value::String(ref v)) => v == "2.0",
            _ => false,
        };
        if !jsonrpc_valid {
            let resp = JsonRpcResponse {
                jsonrpc: "2.0",
                id: req_id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32600,
                    message: "Invalid Request: jsonrpc must be '2.0'".to_string(),
                    data: None,
                }),
            };
            let mut out = serde_json::to_string(&resp)?;
            out.push('\n');
            stdout.write_all(out.as_bytes()).await?;
            stdout.flush().await?;
            continue;
        }

        // Validate method is string
        let method = match raw.method {
            Some(Value::String(ref m)) => m.clone(),
            _ => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: req_id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32600,
                        message: "Invalid Request: method must be a string".to_string(),
                        data: None,
                    }),
                };
                let mut out = serde_json::to_string(&resp)?;
                out.push('\n');
                stdout.write_all(out.as_bytes()).await?;
                stdout.flush().await?;
                continue;
            }
        };

        // Validate params if present (must be object or array)
        if let Some(ref p) = raw.params {
            if !p.is_object() && !p.is_array() && !p.is_null() {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: req_id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32602,
                        message: "Invalid params: must be structured object or array".to_string(),
                        data: None,
                    }),
                };
                let mut out = serde_json::to_string(&resp)?;
                out.push('\n');
                stdout.write_all(out.as_bytes()).await?;
                stdout.flush().await?;
                continue;
            }
        }

        let response = match method.as_str() {
            "initialize" => {
                let requested_version = raw
                    .params
                    .as_ref()
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(|v| v.as_str());

                let negotiated_version = match requested_version {
                    Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v,
                    _ => LATEST_PROTOCOL_VERSION,
                };

                Some(JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: req_id,
                    result: Some(json!({
                        "protocolVersion": negotiated_version,
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": "jma-nowcast-rs",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    })),
                    error: None,
                })
            }
            "ping" => Some(JsonRpcResponse {
                jsonrpc: "2.0",
                id: req_id,
                result: Some(json!({})),
                error: None,
            }),
            "tools/list" => Some(JsonRpcResponse {
                jsonrpc: "2.0",
                id: req_id,
                result: Some(tool_definitions()),
                error: None,
            }),
            "tools/call" => {
                let params = raw.params.unwrap_or(Value::Null);
                let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

                Some(JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: req_id,
                    result: Some(
                        execute_tool(
                            &nowcast,
                            &http_client,
                            &short_range_base_url,
                            tool_name,
                            arguments,
                        )
                        .await,
                    ),
                    error: None,
                })
            }

            _ => Some(JsonRpcResponse {
                jsonrpc: "2.0",
                id: req_id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", method),
                    data: None,
                }),
            }),
        };

        if let Some(resp) = response {
            let mut out = serde_json::to_string(&resp)?;
            out.push('\n');
            stdout.write_all(out.as_bytes()).await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

async fn call_tool(
    nowcast: &Nowcast,
    http_client: &Client,
    short_range_base_url: &str,
    name: &str,
    args: Value,
) -> Result<Value, String> {
    match name {
        "get_precipitation" => {
            let req: PointRequest =
                serde_json::from_value(args).map_err(|e| format!("Invalid arguments: {e}"))?;
            nowcast
                .get_precipitation(req.lat, req.lon, req.zoom)
                .await
                .map_err(|e| e.to_string())
        }
        "get_nowcast" => {
            let req: PointRequest =
                serde_json::from_value(args).map_err(|e| format!("Invalid arguments: {e}"))?;
            nowcast
                .get_nowcast(req.lat, req.lon, req.zoom)
                .await
                .map_err(|e| e.to_string())
        }
        "get_area_nowcast" => {
            let req: AreaRequest =
                serde_json::from_value(args).map_err(|e| format!("Invalid arguments: {e}"))?;
            nowcast
                .get_area_nowcast(
                    req.south,
                    req.west,
                    req.north,
                    req.east,
                    req.zoom,
                    req.lead_minutes,
                )
                .await
                .map_err(|e| e.to_string())
        }
        "get_nearest_rain" => {
            let req: NearestRequest =
                serde_json::from_value(args).map_err(|e| format!("Invalid arguments: {e}"))?;
            nowcast
                .get_nearest_rain(req.lat, req.lon, req.radius_km, req.zoom, req.lead_minutes)
                .await
                .map_err(|e| e.to_string())
        }
        "get_route_nowcast" => {
            let req: RouteRequest =
                serde_json::from_value(args).map_err(|e| format!("Invalid arguments: {e}"))?;
            nowcast
                .get_route_nowcast(req.points, req.zoom)
                .await
                .map_err(|e| e.to_string())
        }
        "get_rain_free_windows" => {
            let req: PointRequest =
                serde_json::from_value(args).map_err(|e| format!("Invalid arguments: {e}"))?;
            nowcast
                .get_rain_free_windows(req.lat, req.lon, req.zoom)
                .await
                .map_err(|e| e.to_string())
        }
        "get_short_range" => {
            let req: PointRequest =
                serde_json::from_value(args).map_err(|e| format!("Invalid arguments: {e}"))?;
            let res = get_short_range(
                http_client,
                short_range_base_url,
                nowcast.now(),
                req.lat,
                req.lon,
                req.zoom,
            )
            .await
            .map_err(|e| e.to_string())?;
            serde_json::to_value(res).map_err(|e| e.to_string())
        }
        _ => Err(format!("Unknown tool: {}", name)),
    }
}

pub(crate) fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "get_precipitation",
                "description": "Get current observed precipitation at a specific latitude/longitude.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "lat": { "type": "number", "description": "Latitude (-85.05 to 85.05)" },
                        "lon": { "type": "number", "description": "Longitude (-180.0 to 180.0)" },
                        "zoom": { "type": "integer", "description": "Web Mercator zoom level (4, 6, 8, 10). Default is 10." }
                    },
                    "required": ["lat", "lon"]
                }
            },
            {
                "name": "get_nowcast",
                "description": "Get precipitation nowcast forecast samples (0 to 60 mins) at a specific latitude/longitude.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "lat": { "type": "number", "description": "Latitude (-85.05 to 85.05)" },
                        "lon": { "type": "number", "description": "Longitude (-180.0 to 180.0)" },
                        "zoom": { "type": "integer", "description": "Web Mercator zoom level (4, 6, 8, 10). Default is 10." }
                    },
                    "required": ["lat", "lon"]
                }
            },
            {
                "name": "get_area_nowcast",
                "description": "Get precipitation histogram/counts over a bounding box area for a specified lead time.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "south": { "type": "number", "description": "Southern latitude bound" },
                        "west": { "type": "number", "description": "Western longitude bound" },
                        "north": { "type": "number", "description": "Northern latitude bound" },
                        "east": { "type": "number", "description": "Eastern longitude bound" },
                        "zoom": { "type": "integer", "description": "Web Mercator zoom level (4, 6, 8, 10). Default is 10." },
                        "lead_minutes": { "type": "integer", "description": "Lead time in minutes (0, 5, 10, ..., 60). Default is 0." }
                    },
                    "required": ["south", "west", "north", "east"]
                }
            },
            {
                "name": "get_nearest_rain",
                "description": "Find the nearest raining pixel within a given search radius (km) from a point.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "lat": { "type": "number", "description": "Latitude (-85.05 to 85.05)" },
                        "lon": { "type": "number", "description": "Longitude (-180.0 to 180.0)" },
                        "radius_km": { "type": "number", "description": "Search radius in kilometers" },
                        "zoom": { "type": "integer", "description": "Web Mercator zoom level (4, 6, 8, 10). Default is 10." },
                        "lead_minutes": { "type": "integer", "description": "Lead time in minutes (0, 5, 10, ..., 60). Default is 0." }
                    },
                    "required": ["lat", "lon", "radius_km"]
                }
            },
            {
                "name": "get_route_nowcast",
                "description": "Evaluate precipitation along a route of time-stamped waypoints.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "points": {
                            "type": "array",
                            "description": "List of waypoints along the route",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "lat": { "type": "number", "description": "Latitude" },
                                    "lon": { "type": "number", "description": "Longitude" },
                                    "time": { "type": "string", "format": "date-time", "description": "ISO 8601 / RFC 3339 timestamp" }
                                },
                                "required": ["lat", "lon", "time"]
                            }
                        },
                        "zoom": { "type": "integer", "description": "Web Mercator zoom level (4, 6, 8, 10). Default is 10." }
                    },
                    "required": ["points"]
                }
            },
            {
                "name": "get_rain_free_windows",
                "description": "Find consecutive dry 5-minute intervals without rain at a given point within the 60-minute forecast.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "lat": { "type": "number", "description": "Latitude (-85.05 to 85.05)" },
                        "lon": { "type": "number", "description": "Longitude (-180.0 to 180.0)" },
                        "zoom": { "type": "integer", "description": "Web Mercator zoom level (4, 6, 8, 10). Default is 10." }
                    },
                    "required": ["lat", "lon"]
                }
            },
            {
                "name": "get_short_range",
                "description": "Get hourly short-range precipitation forecast (R10) combining +1~+6h and +7~+15h series.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "lat": { "type": "number", "description": "Latitude (-85.05 to 85.05)" },
                        "lon": { "type": "number", "description": "Longitude (-180.0 to 180.0)" },
                        "zoom": { "type": "integer", "description": "Web Mercator zoom level (4, 6, 8, 10). Default is 10." }
                    },
                    "required": ["lat", "lon"]
                }
            }
        ]
    })
}

/// Shared tool result envelope for stdio and HTTP transports.
pub(crate) async fn execute_tool(
    nowcast: &Nowcast,
    http_client: &Client,
    short_range_base_url: &str,
    name: &str,
    args: Value,
) -> Value {
    match call_tool(nowcast, http_client, short_range_base_url, name, args).await {
        Ok(value) => json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
            }],
            "structuredContent": value,
            "isError": false
        }),
        Err(error) => json!({
            "content": [{"type": "text", "text": format!("Error: {error}")}],
            "isError": true
        }),
    }
}
