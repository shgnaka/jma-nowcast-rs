"""Deterministic JMA HTTP fixture for black-box acceptance tests."""

import json
import struct
import zlib
from collections import Counter
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread

BASE = datetime(2026, 9, 16, 1, 5, tzinfo=timezone.utc)
FIXED_NOW = "2026-09-16T01:11:00Z"
LONG_BASE = BASE - timedelta(hours=1)
EMPTY_RGBA = (
    Path(__file__)
    .with_name("fixtures")
    .joinpath("20260916010500_z4_x14_y7_empty_rgba.png")
    .read_bytes()
)
PALETTE = bytes((255, 255, 255, 255, 255, 255, 242, 242, 255))


def stamp(value):
    return value.strftime("%Y%m%d%H%M%S")


def iso(value):
    return value.isoformat().replace("+00:00", "Z")


def entry(base, minutes, elements=None):
    return {
        "basetime": stamp(base),
        "validtime": stamp(base + timedelta(minutes=minutes)),
        "elements": ["hrpns", "hrpns_nd"] if elements is None else elements,
    }


def png_tile(default=1, overrides=None):
    pixels = bytearray([default * 17] * (128 * 256))
    for (u, v), value in (overrides or {}).items():
        index = v * 128 + u // 2
        shift = 4 * (1 - u % 2)
        pixels[index] = (pixels[index] & ~(15 << shift)) | (value << shift)

    def chunk(name, content):
        return (
            struct.pack(">I", len(content))
            + name
            + content
            + struct.pack(">I", zlib.crc32(name + content))
        )

    rows = b"".join(b"\0" + pixels[i : i + 128] for i in range(0, len(pixels), 128))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 256, 256, 4, 3, 0, 0, 0))
        + chunk(b"PLTE", PALETTE)
        + chunk(b"tRNS", bytes((0, 0, 255)))
        + chunk(b"IDAT", zlib.compress(rows))
        + chunk(b"IEND", b"")
    )


def tile_path(minutes, x=896, y=405, z=10, base=BASE):
    return (
        f"/{stamp(base)}/none/{stamp(base + timedelta(minutes=minutes))}/surf/hrpns/{z}/{x}/{y}.png"
    )


def short_range_path(member, base, hour):
    return (
        f"/{stamp(base)}/{member}/{stamp(base + timedelta(hours=hour))}/surf/rasrf/10/896/405.png"
    )


class Fixture:
    def __init__(self):
        self.n1 = [entry(BASE - timedelta(minutes=5), 0), entry(BASE, 0)]
        self.n2 = [entry(BASE, minute) for minute in range(60, 0, -5)]
        self.tiles = {
            tile_path(minute): png_tile(
                default={10: 0, 15: 2, 60: 2}.get(minute, 1),
                overrides={(1, 154): 2, (2, 154): 0} if minute == 0 else None,
            )
            for minute in range(0, 61, 5)
        }
        self.rasrf_times = []
        for member, base, hours in [
            ("immed", BASE, range(1, 7)),
            ("none", LONG_BASE, range(7, 16)),
        ]:
            for hour in hours:
                self.rasrf_times.append(dict(entry(base, hour * 60, ["rasrf"]), member=member))
                self.tiles[short_range_path(member, base, hour)] = (
                    EMPTY_RGBA if hour == 3 else png_tile(default={1: 2, 2: 0}.get(hour, 1))
                )
        self.rasrf_times.reverse()
        self.errors = {}
        self.truncate_once = set()
        self.hits = Counter()
        self.url = ""


@contextmanager
def mock_jma():
    fixture = Fixture()

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            path = self.path.split("?", 1)[0]
            fixture.hits[path] += 1
            failures = fixture.errors.get(path)
            if failures:
                status, content = failures.pop(0), b""
            elif path == "/targetTimes_N1.json":
                status, content = 200, json.dumps(fixture.n1).encode()
            elif path == "/targetTimes_N2.json":
                status, content = 200, json.dumps(fixture.n2).encode()
            elif path == "/targetTimes.json":
                status, content = 200, json.dumps(fixture.rasrf_times).encode()
            elif path in fixture.tiles:
                status, content = 200, fixture.tiles[path]
            else:
                status, content = 404, b""
            self.send_response(status)
            self.send_header("Content-Length", str(len(content)))
            self.end_headers()
            if path in fixture.truncate_once:
                fixture.truncate_once.remove(path)
                content = content[: len(content) // 2]
            try:
                self.wfile.write(content)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def log_message(self, *_args):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        fixture.url = f"http://127.0.0.1:{server.server_port}"
        worker = Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            yield fixture
        finally:
            server.shutdown()
            worker.join()
