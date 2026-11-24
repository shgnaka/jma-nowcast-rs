# Third-Party Notices

This project is a Rust implementation informed by the `jma-radar` reference
implementation. The tile/Web Mercator constants, palette, and decoding rules in
`src/tiles.rs`, `src/decode.rs`, and `src/levels.rs` were reviewed against that
project.

## jma-radar

- Source: <https://github.com/ringsaturn/jma-radar>
- License: MIT
- Copyright (c) 2026 ringsaturn

The full MIT license text from the reference implementation is reproduced
below.

```text
MIT License

Copyright (c) 2026 ringsaturn

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Rust dependencies

The crate uses the following Rust crates, each under its own upstream license:

- `png` — MIT OR Apache-2.0
- `serde` — MIT OR Apache-2.0
- `serde_json` (development only) — MIT OR Apache-2.0
