"""End-to-end acceptance tests against a loopback JMA server.

Run with python3 -m unittest discover -s tests -p acceptance.py after cargo build.
"""

import json
import math
import os
import socket
import subprocess
import time
import unittest
from contextlib import contextmanager
from datetime import timedelta
from http.client import HTTPConnection
from pathlib import Path

from mock_jma import (
    BASE,
    FIXED_NOW,
    LONG_BASE,
    entry,
    iso,
    mock_jma,
    png_tile,
    short_range_path,
    tile_path,
)

BINARY = Path(os.environ.get("NOWCAST_BINARY", "target/debug/jma-nowcast-rs")).resolve()


def environment(fixture):
    return dict(
        os.environ,
        JMA_BASE_URL=fixture.url,
        JMA_SHORT_RANGE_BASE_URL=fixture.url,
        JMA_FIXED_NOW=FIXED_NOW,
    )


def run(fixture, *args, stdin=None):
    return subprocess.run(
        [BINARY, *args],
        check=False,
        env=environment(fixture),
        input=stdin,
        text=True,
        capture_output=True,
        timeout=20,
    )


def center(u, v):
    world = 256 * 1024
    lon = ((896 * 256 + u + 0.5) / world) * 360 - 180
    lat = math.degrees(math.atan(math.sinh(math.pi * (1 - 2 * (405 * 256 + v + 0.5) / world))))
    return str(lat), str(lon)


@contextmanager
def rest_server(fixture):
    with socket.socket() as reserved:
        reserved.bind(("127.0.0.1", 0))
        port = reserved.getsockname()[1]
    process = subprocess.Popen(
        [BINARY, "serve", "--bind", f"127.0.0.1:{port}"],
        env=environment(fixture),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 5
        while process.poll() is None and time.monotonic() < deadline:
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                    break
            except OSError:
                time.sleep(0.05)
        else:
            if process.poll() is None:
                process.terminate()
            _, stderr = process.communicate(timeout=5)
            raise RuntimeError(f"REST startup failed: {stderr}")
        yield port
    finally:
        if process.poll() is None:
            process.terminate()
        try:
            process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate()


def post(port, route, body):
    connection = HTTPConnection("127.0.0.1", port, timeout=15)
    try:
        connection.request(
            "POST", f"/v1/{route}", json.dumps(body), {"Content-Type": "application/json"}
        )
        response = connection.getresponse()
        return response.status, json.load(response)
    finally:
        connection.close()


class Acceptance(unittest.TestCase):
    def cli(self, fixture, command, *args, point=("35", "135")):
        result = run(fixture, command, f"--lat={point[0]}", f"--lon={point[1]}", *args)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        return json.loads(result.stdout)

    def test_latest_observation_distinguishes_missing_dry_and_rain(self):
        with mock_jma() as fixture:
            for u, level, status in [(0, 1, "no_rain"), (1, 2, "rain"), (2, 0, "no_data")]:
                point = self.cli(fixture, "precipitation", point=center(u, 154))
                self.assertEqual(point["precipitation"]["level"], level)
                self.assertEqual(point["precipitation"]["status"], status)
                self.assertEqual(point["basetime"], iso(BASE))
                self.assertEqual(point["validtime"], iso(BASE))
                self.assertEqual(point["source_url"], "https://www.jma.go.jp/bosai/nowc/")
                self.assertIs(point["processed"], True)
                self.assertTrue(point["processing"])
                self.assertEqual(point["license"], "PDL-1.0")
            self.assertEqual(fixture.hits[tile_path(0)], 3)
            self.assertEqual(fixture.hits["/targetTimes_N2.json"], 0)

    def test_all_slots_use_latest_complete_matching_cycle(self):
        with mock_jma() as fixture:
            newer = BASE + timedelta(minutes=5)
            fixture.n1.append(entry(newer, 0))
            fixture.n2.insert(2, fixture.n2[0].copy())
            fixture.n2.extend((entry(newer, 5), entry(BASE, 20, ["hrpns_nd"])))
            result = self.cli(fixture, "nowcast")
            self.assertEqual(result["basetime"], iso(BASE))
            self.assertEqual(
                [sample["validtime"] for sample in result["samples"]],
                [iso(BASE + timedelta(minutes=minute)) for minute in range(0, 61, 5)],
            )
            self.assertEqual(
                [sample["precipitation"]["level"] for sample in result["samples"]],
                [1, 1, 0, 2, 1, 1, 1, 1, 1, 1, 1, 1, 2],
            )
            self.assertEqual(
                {path: count for path, count in fixture.hits.items() if path.endswith(".png")},
                {tile_path(minute): 1 for minute in range(0, 61, 5)},
            )

    def test_missing_slot_stale_cycle_and_future_cycle_fail_before_tile_fetch(self):
        with mock_jma() as fixture:
            fixture.n2.pop(4)
            missing = run(fixture, "nowcast", "--lat=35", "--lon=135")
            self.assertNotEqual(missing.returncode, 0)
            for cycle in [BASE - timedelta(minutes=21), BASE + timedelta(minutes=10)]:
                fixture.n1 = [entry(cycle, 0)]
                fixture.n2 = [entry(cycle, minute) for minute in range(5, 61, 5)]
                result = run(fixture, "nowcast", "--lat=35", "--lon=135")
                self.assertNotEqual(result.returncode, 0)
            self.assertFalse(any(path.endswith(".png") for path in fixture.hits))

    def test_non_hrpns_n2_entry_cannot_complete_forecast(self):
        with mock_jma() as fixture:
            fixture.n2 = [
                entry(BASE, minute, ["hrpns_nd"] if minute == 15 else ["hrpns"])
                for minute in range(5, 61, 5)
            ]
            fixture.n1.append(entry(BASE, 15))
            result = run(fixture, "nowcast", "--lat=35", "--lon=135")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing_times", result.stderr)
            self.assertFalse(any(path.endswith(".png") for path in fixture.hits))

    def test_malformed_advertised_timestamp_is_a_structured_error(self):
        with mock_jma() as fixture, rest_server(fixture) as port:
            fixture.n1 = [
                {
                    "basetime": "205年09160105",
                    "validtime": "205年09160105",
                    "elements": ["hrpns"],
                }
            ]
            status, error = post(port, "precipitation", {"lat": 35, "lon": 135})
            self.assertEqual(status, 502, error)
            self.assertIn("upstream_data", json.dumps(error))
            self.assertFalse(any(path.endswith(".png") for path in fixture.hits))

    def test_503_retries_and_404_is_not_reported_as_a_weather_sample(self):
        with mock_jma() as fixture:
            path = tile_path(0)
            fixture.errors[path] = [503]
            self.cli(fixture, "precipitation")
            self.assertEqual(fixture.hits[path], 2)
            fixture.errors[path] = [404, 404, 404]
            failed = run(fixture, "precipitation", "--lat=35", "--lon=135")
            self.assertNotEqual(failed.returncode, 0)
            self.assertEqual(fixture.hits[path], 3)
            self.assertIn("404", failed.stderr + failed.stdout)

    def test_transfer_failure_retries_and_invalid_tile_is_not_cached(self):
        with mock_jma() as fixture, rest_server(fixture) as port:
            body = {"lat": 35, "lon": 135}
            path = tile_path(0)
            image = fixture.tiles[path]
            fixture.tiles[path] = b"corrupt tile"
            status, error = post(port, "precipitation", body)
            self.assertEqual(status, 502, error)
            self.assertIn("upstream_data", json.dumps(error))
            self.assertEqual(fixture.hits[path], 1)

            fixture.tiles[path] = image
            fixture.truncate_once.add(path)
            status, result = post(port, "precipitation", body)
            self.assertEqual(status, 200, result)
            self.assertEqual(result["precipitation"]["level"], 1)
            self.assertEqual(fixture.hits[path], 3)

    def test_rain_free_windows_split_on_missing_and_exclude_rain(self):
        with mock_jma() as fixture:
            result = self.cli(fixture, "rain-free-windows")
            self.assertEqual(
                [(window["start"], window["end"]) for window in result["windows"]],
                [
                    (iso(BASE), iso(BASE + timedelta(minutes=10))),
                    (iso(BASE + timedelta(minutes=20)), iso(BASE + timedelta(minutes=60))),
                ],
            )

    def test_lon_wrap_does_not_hide_nearest_rain(self):
        with mock_jma() as fixture, rest_server(fixture) as port:
            lat, _ = map(float, center(0, 154))
            fixture.tiles[tile_path(0, x=1023)] = png_tile()
            fixture.tiles[tile_path(0, x=0)] = png_tile(overrides={(0, 154): 2})
            status, result = post(
                port,
                "nearest-rain",
                {
                    "lat": lat,
                    "lon": 179.999,
                    "radius_km": 0.32,
                },
            )
            self.assertEqual(status, 200, result)
            self.assertIsNotNone(result["nearest"])
            self.assertAlmostEqual(result["nearest"]["bearing_degrees"], 90, delta=0.1)
            self.assertAlmostEqual(result["nearest"]["distance_km"], 0.154, delta=0.004)

    def test_dry_endpoint_has_no_positive_duration_window(self):
        with mock_jma() as fixture:
            for minute in range(0, 60, 5):
                fixture.tiles[tile_path(minute)] = png_tile(default=2)
            fixture.tiles[tile_path(60)] = png_tile()
            result = self.cli(fixture, "rain-free-windows")
            self.assertEqual(result["windows"], [])

    def test_rest_area_nearest_and_route(self):
        with mock_jma() as fixture, rest_server(fixture) as port:
            lat, west = map(float, center(0, 154))
            _, east = map(float, center(2, 154))
            status, area = post(
                port,
                "area-nowcast",
                {
                    "south": lat,
                    "west": west,
                    "north": lat,
                    "east": east,
                },
            )
            self.assertEqual(status, 200, area)
            self.assertEqual(area["counts"], [1, 1, 1, 0, 0, 0, 0, 0, 0, 0])

            status, nearest = post(
                port,
                "nearest-rain",
                {
                    "lat": lat,
                    "lon": east,
                    "radius_km": 0.19,
                },
            )
            self.assertEqual(status, 200, nearest)
            self.assertGreaterEqual(nearest["missing_pixels"], 1)
            self.assertAlmostEqual(nearest["nearest"]["distance_km"], 0.125, delta=0.003)
            self.assertAlmostEqual(nearest["nearest"]["bearing_degrees"], 270, delta=0.01)

            _, distant = map(float, center(20, 154))
            status, none = post(
                port,
                "nearest-rain",
                {
                    "lat": lat,
                    "lon": distant,
                    "radius_km": 0.19,
                },
            )
            self.assertEqual(status, 200, none)
            self.assertIsNone(none["nearest"])
            self.assertEqual(none["missing_pixels"], 0)

            expected = [(9, 5, 1), (10, 10, 0), (60, 60, 2)]
            status, route = post(
                port,
                "route-nowcast",
                {
                    "points": [
                        {"lat": lat, "lon": west, "time": iso(BASE + timedelta(minutes=requested))}
                        for requested, _, _ in expected
                    ],
                },
            )
            self.assertEqual(status, 200, route)
            self.assertEqual(
                [
                    (sample["validtime"], sample["precipitation"]["level"])
                    for sample in route["samples"]
                ],
                [(iso(BASE + timedelta(minutes=lead)), level) for _, lead, level in expected],
            )
            self.assertEqual(fixture.hits[tile_path(0)], 1)
            for invalid_minutes in [-1, 61]:
                status, _ = post(
                    port,
                    "route-nowcast",
                    {
                        "points": [
                            {
                                "lat": lat,
                                "lon": west,
                                "time": iso(BASE + timedelta(minutes=invalid_minutes)),
                            }
                        ],
                    },
                )
                self.assertEqual(status, 400)

    def test_rest_rejects_oversized_or_invalid_area_before_fetch_and_does_not_cache_failures(self):
        with mock_jma() as fixture, rest_server(fixture) as port:
            for bounds in [
                {"south": 25, "west": 120, "north": 45, "east": 145},
                {"south": 34, "west": 179, "north": 35, "east": -179},
            ]:
                status, error = post(port, "area-nowcast", bounds)
                self.assertEqual(status, 400, error)
            status, error = post(port, "precipitation", {"lat": "invalid", "lon": 135})
            self.assertEqual(status, 400, error)
            self.assertEqual(error["code"], "invalid_input")
            self.assertFalse(any(path.endswith(".png") for path in fixture.hits))
            path = tile_path(0)
            fixture.errors[path] = [404]
            body = {"lat": 35, "lon": 135}
            status, error = post(port, "precipitation", body)
            self.assertEqual(status, 502, error)
            self.assertIn("upstream_not_found", json.dumps(error))
            self.assertEqual(fixture.hits[path], 1)
            for _ in range(2):
                status, result = post(port, "precipitation", body)
                self.assertEqual(status, 200, result)
                self.assertEqual(result["precipitation"]["level"], 1)
            self.assertEqual(fixture.hits[path], 2)
            status, forecast = post(port, "nowcast", body)
            self.assertEqual(status, 200, forecast)
            self.assertEqual(forecast["samples"][1]["precipitation"]["level"], 1)
            self.assertEqual(fixture.hits[tile_path(5)], 1)
            self.assertEqual(fixture.hits[path], 2)
            newer = BASE + timedelta(minutes=5)
            fixture.n1.append(entry(newer, 0))
            fixture.tiles[tile_path(0, base=newer)] = fixture.tiles[tile_path(15)]
            status, result = post(port, "precipitation", body)
            self.assertEqual(status, 200, result)
            self.assertEqual(result["precipitation"]["level"], 2)
            self.assertEqual(result["basetime"], iso(newer))

    def test_short_range_preserves_separate_cycles_and_all_accumulation_windows(self):
        with mock_jma() as fixture:
            incomplete = BASE + timedelta(minutes=5)
            fixture.rasrf_times.append(dict(entry(incomplete, 60, ["rasrf"]), member="immed"))
            result = self.cli(fixture, "short-range")
            self.assertEqual(result["unit"], "mm")
            self.assertEqual(result["source_url"], "https://www.jma.go.jp/bosai/kaikotan/")
            self.assertIs(result["processed"], True)
            paths = {}
            for member, base, hours in [
                ("immed", BASE, range(1, 7)),
                ("none", LONG_BASE, range(7, 16)),
            ]:
                series = result[member]
                self.assertEqual(series["basetime"], iso(base))
                self.assertEqual(series["member"], member)
                self.assertEqual(len(series["samples"]), len(hours))
                for sample, hour in zip(series["samples"], hours):
                    self.assertEqual(sample["validtime"], iso(base + timedelta(hours=hour)))
                    self.assertEqual(
                        sample["accumulation_start"], iso(base + timedelta(hours=hour - 1))
                    )
                    self.assertEqual(sample["accumulation_end"], sample["validtime"])
                    paths[short_range_path(member, base, hour)] = 1
            samples = result["immed"]["samples"]
            self.assertEqual(
                [
                    (sample["level"], sample["status"], sample["min_mm"], sample["max_mm"])
                    for sample in samples[:4]
                ],
                [
                    (2, "rain", 0, 1),
                    (0, "no_data", None, None),
                    (0, "unavailable_or_below_display_threshold", None, None),
                    (1, "below_display_threshold", None, None),
                ],
            )
            self.assertEqual(
                {path: hits for path, hits in fixture.hits.items() if path.endswith(".png")}, paths
            )

    def test_short_range_advertised_overlap_may_omit_only_covered_prefix(self):
        with mock_jma() as fixture:
            missing_lead_7 = entry(LONG_BASE, 7 * 60, ["rasrf"])["validtime"]
            fixture.rasrf_times = [
                item
                for item in fixture.rasrf_times
                if (item["member"], item["validtime"]) != ("none", missing_lead_7)
            ]
            result = self.cli(fixture, "short-range")
            self.assertEqual(len(result["immed"]["samples"]), 6)
            self.assertEqual(len(result["none"]["samples"]), 8)
            self.assertEqual(result["none"]["basetime"], iso(LONG_BASE))
            self.assertEqual(
                result["none"]["samples"][0]["validtime"], iso(LONG_BASE + timedelta(hours=8))
            )
            self.assertNotIn(short_range_path("none", LONG_BASE, 7), fixture.hits)

            previous_tile_hits = {
                path: hits for path, hits in fixture.hits.items() if path.endswith(".png")
            }
            missing_lead_8 = entry(LONG_BASE, 8 * 60, ["rasrf"])["validtime"]
            fixture.rasrf_times = [
                item
                for item in fixture.rasrf_times
                if (item["member"], item["validtime"]) != ("none", missing_lead_8)
            ]
            failed = run(fixture, "short-range", "--lat=35", "--lon=135")
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("missing_times", failed.stderr)
            self.assertEqual(
                {path: hits for path, hits in fixture.hits.items() if path.endswith(".png")},
                previous_tile_hits,
            )

            earlier_base = BASE - timedelta(hours=2)
            fixture.rasrf_times = [
                item for item in fixture.rasrf_times if item["member"] == "immed"
            ]
            fixture.rasrf_times.extend(
                dict(entry(earlier_base, hour * 60, ["rasrf"]), member="none")
                for hour in range(7, 16)
                if hour != 8
            )
            failed = run(fixture, "short-range", "--lat=35", "--lon=135")
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("missing_times", failed.stderr)
            self.assertEqual(
                {path: hits for path, hits in fixture.hits.items() if path.endswith(".png")},
                previous_tile_hits,
            )

    def test_short_range_none_member_can_supply_both_series(self):
        with mock_jma() as fixture:
            fixture.rasrf_times = [
                dict(entry(BASE, hour * 60, ["rasrf"]), member="none") for hour in range(15, 0, -1)
            ]
            expected_paths = {short_range_path("none", BASE, hour) for hour in range(1, 16)}
            fixture.tiles.update({path: png_tile() for path in expected_paths})
            result = self.cli(fixture, "short-range")
            for group, count in [("immed", 6), ("none", 9)]:
                series = result[group]
                self.assertEqual(series["member"], "none")
                self.assertEqual(series["basetime"], iso(BASE))
                self.assertEqual(len(series["samples"]), count)
            self.assertEqual(
                {
                    path
                    for path, hits in fixture.hits.items()
                    if path.endswith(".png") and hits == 1
                },
                expected_paths,
            )

    def test_short_range_missing_stale_and_404_are_structured_errors(self):
        with mock_jma() as fixture, rest_server(fixture) as port:
            original = fixture.rasrf_times[:]
            payload = {"lat": 35, "lon": 135}
            fixture.rasrf_times.pop(0)
            status, error = post(port, "short-range", payload)
            self.assertEqual(status, 502, error)
            self.assertIn("missing_times", json.dumps(error))

            fixture.rasrf_times = [item for item in original if item["member"] == "none"]
            expired = BASE - timedelta(minutes=41)
            fixture.rasrf_times.extend(
                dict(entry(expired, hour * 60, ["rasrf"]), member="immed") for hour in range(1, 7)
            )
            status, error = post(port, "short-range", payload)
            self.assertEqual(status, 502, error)
            self.assertIn("stale_data", json.dumps(error))
            self.assertFalse(any(path.endswith(".png") for path in fixture.hits))

            fixture.rasrf_times = original
            first = short_range_path("immed", BASE, 1)
            fixture.errors[first] = [404]
            status, error = post(port, "short-range", payload)
            self.assertEqual(status, 502, error)
            self.assertIn("upstream_not_found", json.dumps(error))
            self.assertEqual(fixture.hits[first], 1)

    def test_stdio_mcp_lists_and_calls_tools(self):
        def message(method, request_id=None, params=None):
            result = {"jsonrpc": "2.0", "method": method}
            if request_id is not None:
                result["id"] = request_id
            if params is not None:
                result["params"] = params
            return result

        requests = [
            message(
                "initialize",
                10,
                {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "acceptance", "version": "1"},
                },
            ),
            message("notifications/initialized"),
            message("ping"),
            message("tools/list", 11),
            message(
                "tools/call",
                12,
                {"name": "get_precipitation", "arguments": {"lat": 35, "lon": 135}},
            ),
            message(
                "tools/call",
                13,
                {"name": "get_precipitation", "arguments": {"lat": 95, "lon": 135}},
            ),
            message("unrecognized/method", 14),
            message(
                "tools/call",
                15,
                {"name": "get_precipitation", "arguments": {"lat": 35, "lon": 135, "zoom": 266}},
            ),
            message(
                "tools/call", 16, {"name": "get_short_range", "arguments": {"lat": 35, "lon": 135}}
            ),
        ]
        with mock_jma() as fixture:
            completed = run(fixture, "mcp", stdin="".join(json.dumps(i) + "\n" for i in requests))
            self.assertEqual(completed.returncode, 0, completed.stderr)
            lines = [json.loads(line) for line in completed.stdout.splitlines()]
            self.assertEqual(len(lines), 7)
            responses = {line["id"]: line for line in lines}
            self.assertEqual(set(responses), {10, 11, 12, 13, 14, 15, 16})
            self.assertIn("tools", responses[10]["result"]["capabilities"])
            self.assertEqual(responses[10]["result"]["protocolVersion"], "2025-11-25")
            tools = {tool["name"]: tool["inputSchema"] for tool in responses[11]["result"]["tools"]}
            expected = {
                "get_precipitation",
                "get_nowcast",
                "get_area_nowcast",
                "get_nearest_rain",
                "get_route_nowcast",
                "get_rain_free_windows",
                "get_short_range",
            }
            self.assertTrue(expected.issubset(tools))
            self.assertEqual(tools["get_precipitation"]["type"], "object")
            self.assertTrue({"lat", "lon"}.issubset(tools["get_precipitation"]["required"]))
            contents = responses[12]["result"]
            self.assertFalse(contents.get("isError", False))
            actual = json.loads(contents["content"][0]["text"])
            self.assertEqual(actual["precipitation"]["level"], 1)
            self.assertTrue(responses[13]["result"]["isError"])
            self.assertEqual(responses[14]["error"]["code"], -32601)
            self.assertTrue(responses[15]["result"]["isError"])
            self.assertEqual(fixture.hits[tile_path(0)], 1)
            short_range = responses[16]["result"]
            self.assertFalse(short_range.get("isError", False))
            self.assertEqual(json.loads(short_range["content"][0]["text"])["unit"], "mm")


if __name__ == "__main__":
    unittest.main()
