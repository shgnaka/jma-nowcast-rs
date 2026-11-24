# jma-nowcast-rs

Rust implementation of Japan Meteorological Agency (JMA / 気象庁) High-Resolution
Precipitation Nowcast (高解像度降水ナウキャスト) processing, plus the separate
hourly short-range precipitation forecast (今後の雨 / `rasrf`). The crate ships a
CLI, a REST API, and a Model Context Protocol (MCP) stdio server over one shared
query engine.

## Features

- **Web Mercator XYZ tiling**: 256×256 tiles, zoom levels 4, 6, 8, 10 (default 10).
- **Indexed PNG decoding**: 4-bit indexed PNG tiles mapped to JMA precipitation
  levels (0 to 9) and mm/h intervals.
- **Seven shared queries**:
  - `precipitation`: current observed precipitation at a coordinate.
  - `nowcast`: 0–60 minute forecast in 5-minute steps.
  - `area-nowcast`: precipitation histogram across a bounding box.
  - `nearest-rain`: nearest raining pixel (level ≥ 2) within a radius, with distance and bearing.
  - `route-nowcast`: precipitation evaluation across time-stamped route waypoints.
  - `rain-free-windows`: continuous dry intervals within the 60-minute horizon.
  - `short-range`: hourly +1 h…+15 h accumulation forecast in millimetres, with
    independent `immed` and `none` basetimes. JMA may omit the leading `none`
    frames whose end times are already covered by the latest `immed` series.
- **Multiple interfaces**: CLI, REST API (axum), and stdio MCP server.
- **Deterministic testing**: every interface honours `JMA_BASE_URL`,
  `JMA_SHORT_RANGE_BASE_URL`, and `JMA_FIXED_NOW`.

## Build, Test, and Install

The Rust toolchain is the only build requirement. Python 3 is needed only for
the end-to-end acceptance tests.

```bash
# Debug build and unit/integration tests
cargo build
cargo test

# End-to-end acceptance tests against the loopback JMA mock
python3 -m unittest discover -s tests -p acceptance.py

# Release build and installation
cargo build --release
cargo install --path .
```

The executable is named `jma-nowcast-rs`. After `cargo install --path .` it is
available on `PATH`; otherwise run it as `target/debug/jma-nowcast-rs` or
`target/release/jma-nowcast-rs`.

## CLI Usage

All successful commands write a single JSON document to stdout. Failures write a
JSON object with `error` and `code` to stderr and exit with a nonzero status.

```bash
# Point observation
jma-nowcast-rs precipitation --lat 35.6812 --lon 139.7671 --zoom 10

# Forecast samples (0–60 minutes)
jma-nowcast-rs nowcast --lat 35.6812 --lon 139.7671

# Area histogram
jma-nowcast-rs area-nowcast --south 35.5 --west 139.5 --north 35.8 --east 139.8 --lead-minutes 15

# Nearest rain within 25 km
jma-nowcast-rs nearest-rain --lat 35.6812 --lon 139.7671 --radius-km 25.0

# Route evaluation
jma-nowcast-rs route-nowcast --points '[{"lat":35.68,"lon":139.76,"time":"2026-11-24T07:00:00Z"}]'

# Dry windows
jma-nowcast-rs rain-free-windows --lat 35.6812 --lon 139.7671

# Hourly short-range forecast (+1 h…+15 h)
jma-nowcast-rs short-range --lat 35.6812 --lon 139.7671

# REST server
jma-nowcast-rs serve --bind 127.0.0.1:8080

# MCP stdio server
jma-nowcast-rs mcp
```

## REST API

The server binds to `127.0.0.1:8080` by default, so it is not exposed publicly
unless you deliberately change `--bind`. Every endpoint is `POST` and accepts a
JSON request body. Invalid request bodies and handler errors are returned as
JSON with `code` and `error` fields; invalid input is HTTP 400 and upstream
failures are HTTP 502.

- `POST /v1/precipitation` — `{ "lat": 35.68, "lon": 139.76, "zoom": 10 }`
- `POST /v1/nowcast` — `{ "lat": 35.68, "lon": 139.76, "zoom": 10 }`
- `POST /v1/area-nowcast` — `{ "south": 35.5, "west": 139.5, "north": 35.8, "east": 139.8, "zoom": 10, "lead_minutes": 10 }`
- `POST /v1/nearest-rain` — `{ "lat": 35.68, "lon": 139.76, "radius_km": 20.0, "zoom": 10, "lead_minutes": 0 }`
- `POST /v1/route-nowcast` — `{ "points": [{"lat": 35.68, "lon": 139.76, "time": "2026-11-24T07:00:00Z"}], "zoom": 10 }`
- `POST /v1/rain-free-windows` — `{ "lat": 35.68, "lon": 139.76, "zoom": 10 }`
- `POST /v1/short-range` — `{ "lat": 35.68, "lon": 139.76, "zoom": 10 }`

Example:

```bash
curl -sS http://127.0.0.1:8080/v1/precipitation \
  -H 'Content-Type: application/json' \
  -d '{"lat":35.6812,"lon":139.7671}'
```

## Model Context Protocol (MCP)

Runs over standard input/output (stdio) with JSON-RPC 2.0. Supports
`initialize`, `ping`, `tools/list`, and `tools/call`. The server negotiates MCP
protocol version `2025-11-25`, does not reply to notifications, and returns
tool failures as `isError` results.

Tools provided:

- `get_precipitation`
- `get_nowcast`
- `get_area_nowcast`
- `get_nearest_rain`
- `get_route_nowcast`
- `get_rain_free_windows`
- `get_short_range`

Example client configuration:

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

## Environment Variables

- `JMA_BASE_URL`: base URL for nowcast metadata and `hrpns` tiles. Default:
  `https://www.jma.go.jp/bosai/jmatile/data/nowc`.
- `JMA_SHORT_RANGE_BASE_URL`: base URL for `rasrf/targetTimes.json` and hourly
  short-range tiles. Default: `https://www.jma.go.jp/bosai/jmatile/data/rasrf`.
- `JMA_FIXED_NOW`: RFC 3339 timestamp (for example
  `2026-11-24T07:00:00Z`) used as the clock for every query. This is intended for
  deterministic tests; leave it unset in production so the current UTC time is
  used.

Both base URLs can point at a loopback test server. They must include the
product directory that contains `targetTimes_N1.json` / `targetTimes_N2.json`
(nowcast) or `targetTimes.json` (short range).

## Running on a VPS

Keep the REST server on the VPS loopback interface and reach it through an SSH
tunnel or a reverse proxy on the same host. Binding to `127.0.0.1` avoids
exposing an unauthenticated service to the internet.

```bash
# On the VPS, from the repository checkout
cargo build --release
./target/release/jma-nowcast-rs serve --bind 127.0.0.1:8080

# Optional: run it as a background service
nohup ./target/release/jma-nowcast-rs serve --bind 127.0.0.1:8080 \
  >/tmp/jma-nowcast.log 2>&1 &

# From your workstation, forward the loopback port
ssh -N -L 8080:127.0.0.1:8080 user@vps.example.com

# Then call the API locally
curl -sS http://127.0.0.1:8080/v1/short-range \
  -H 'Content-Type: application/json' \
  -d '{"lat":35.6812,"lon":139.7671}'
```

If a reverse proxy is required, terminate TLS at the proxy and forward only to
`127.0.0.1:8080`; do not publish port 8080 on a public interface. The CLI and
MCP server do not listen on a network socket and can be run directly on the VPS
or locally.

## Attribution & License

Data provided by the Japan Meteorological Agency (気象庁), processed and
distributed under the Government of Japan Standard Terms of Use (PDL-1.0).
Source code is licensed under the MIT License.
