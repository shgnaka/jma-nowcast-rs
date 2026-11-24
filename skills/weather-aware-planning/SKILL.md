---
name: weather-aware-planning
description: Weather-aware schedule and outdoor route planning using JMA high-resolution precipitation nowcast and hourly short-range forecasts.
---

# Weather-Aware Planning

This skill assists users and agents in planning outdoor activities, travel
routes, and event timing by querying Japan Meteorological Agency (JMA)
high-resolution precipitation nowcasts and hourly short-range forecasts through
the `jma-nowcast-rs` CLI, REST API, or MCP tools.

## Available Capabilities & MCP Tools

The service provides seven shared queries:

1. **`get_precipitation(lat, lon, zoom=10)`**: Check current precipitation at coordinates.
2. **`get_nowcast(lat, lon, zoom=10)`**: Inspect precipitation in 5-minute steps for the next 60 minutes.
3. **`get_area_nowcast(south, west, north, east, zoom=10, lead_minutes=0)`**: Analyze rain coverage over a bounding box as counts per precipitation class.
4. **`get_nearest_rain(lat, lon, radius_km, zoom=10, lead_minutes=0)`**: Find the closest rain within `radius_km`, including distance and the compass bearing of its position relative to the query point; this bearing does not indicate rain movement.
5. **`get_route_nowcast(points=[{lat, lon, time}], zoom=10)`**: Evaluate weather along a timed route.
6. **`get_rain_free_windows(lat, lon, zoom=10)`**: Identify continuous dry time windows in the 60-minute nowcast.
7. **`get_short_range(lat, lon, zoom=10)`**: Get the hourly +1 h…+15 h forecast. Values are one-hour accumulations in **mm**, not mm/h; use each series' `basetime` and `member`. JMA can publish both series under member `none` or omit leading extended frames covered by the +1 h…+6 h series.

All queries return source attribution, a processed flag, processing notes, and
the PDL-1.0 licence. Nowcast precipitation classes use JMA levels 0–9; level 0
means missing data, level 1 means no rain, and levels 2–9 are rain intensity
classes. R10 short-range has its own display threshold and statuses, described
under Multi-Hour Planning.

## Workflows

### 1. Checking Current & Immediate Rain at a Location
When an outdoor activity is imminent (e.g. going for a walk, cycling):
- Call `get_precipitation(lat, lon)` to see if it is currently raining.
- Call `get_rain_free_windows(lat, lon)` to determine the best dry departure window.
- If rain is threatening, use `get_nearest_rain(lat, lon, radius_km)` to find the distance and compass bearing from the point to the nearest raining pixel.

### 2. Route Planning & Commuting
When planning travel across multiple waypoints:
- Construct waypoints with expected arrival times: `[{"lat": 35.6812, "lon": 139.7671, "time": "2026-11-24T08:00:00Z"}, ...]`.
- Call `get_route_nowcast(points, zoom)` to check the forecast precipitation class at each waypoint and its sampled time.
- Waypoint times must fall within the selected nowcast's 0–60 minute window.
- Adjust travel timing or route accordingly.

### 3. Event & Area Weather Monitoring
For outdoor events covering a geographic area (parks, stadiums, construction sites):
- Call `get_area_nowcast(south, west, north, east, lead_minutes)` for `lead_minutes = 0, 15, 30, 45, 60`.
- Monitor counts across precipitation levels to estimate rain coverage over time.
- Area queries reject rectangles wider or taller than 10°, crossing the antimeridian, or covering over 200,000 pixels before fetching tiles.

### 4. Multi-Hour Planning
When the planning horizon extends beyond the 60-minute nowcast:
- Call `get_short_range(lat, lon)` for hourly accumulations from +1 h to +15 h.
- Use `accumulation_start` and `accumulation_end` to identify the hour covered by each value in millimetres.
- Check status values: `rain`, `no_data`, `below_display_threshold`, or `unavailable_or_below_display_threshold`.
- Remember that transparent tiles cannot distinguish missing data from an accumulation below the display threshold.

## Running the Service

### CLI
Successful commands print JSON to stdout. Errors print JSON with `error` and
`code` to stderr and exit nonzero.

```bash
# Point precipitation observation
cargo run -- precipitation --lat 35.6812 --lon 139.7671

# 60-minute nowcast
cargo run -- nowcast --lat 35.6812 --lon 139.7671

# Nearest rain within 20 km
cargo run -- nearest-rain --lat 35.6812 --lon 139.7671 --radius-km 20.0

# Dry windows
cargo run -- rain-free-windows --lat 35.6812 --lon 139.7671

# Hourly forecast (+1 h…+15 h)
cargo run -- short-range --lat 35.6812 --lon 139.7671
```

After `cargo install --path .`, the executable is `jma-nowcast-rs`. Use the same
subcommands, for example `jma-nowcast-rs short-range --lat 35.6812 --lon 139.7671`.

### REST Server
```bash
cargo run -- serve --bind 127.0.0.1:8080
```

All seven endpoints are `POST` with JSON bodies. Invalid requests return JSON
errors (HTTP 400 for input, HTTP 502 for upstream failures).

- `POST /v1/precipitation`
- `POST /v1/nowcast`
- `POST /v1/area-nowcast`
- `POST /v1/nearest-rain`
- `POST /v1/route-nowcast`
- `POST /v1/rain-free-windows`
- `POST /v1/short-range`

### MCP Server (stdio)
Configure it in your MCP client settings (for example Claude Desktop or
Antigravity CLI). The server negotiates MCP `2025-11-25`, does not respond to
notifications, and exposes seven tools, including `get_short_range`.

```json
{
  "mcpServers": {
    "jma-nowcast-rs": {
      "command": "jma-nowcast-rs",
      "args": ["mcp"],
      "env": {
        "JMA_BASE_URL": "https://www.jma.go.jp/bosai/jmatile/data/nowc",
        "JMA_SHORT_RANGE_BASE_URL": "https://www.jma.go.jp/bosai/jmatile/data/rasrf"
      }
    }
  }
}
```

### Environment Variables

- `JMA_BASE_URL`: nowcast metadata and `hrpns` tile base. Default `https://www.jma.go.jp/bosai/jmatile/data/nowc`.
- `JMA_SHORT_RANGE_BASE_URL`: `rasrf` metadata and tile base. Default `https://www.jma.go.jp/bosai/jmatile/data/rasrf`.
- `JMA_FIXED_NOW`: RFC 3339 timestamp used as the clock for deterministic testing.

### VPS Notes

Run the REST server on the VPS loopback interface, then connect through an SSH
tunnel rather than exposing it publicly:

```bash
# On the VPS
./target/release/jma-nowcast-rs serve --bind 127.0.0.1:8080

# On your workstation
ssh -N -L 8080:127.0.0.1:8080 user@vps.example.com
curl -sS http://127.0.0.1:8080/v1/precipitation \
  -H 'Content-Type: application/json' \
  -d '{"lat":35.6812,"lon":139.7671}'
```
