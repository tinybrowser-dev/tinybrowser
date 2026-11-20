# tinybrowser

A small, spec-first headless browser for AI agents.

One ~12 MiB Rust binary (Blitz, Stylo, html5ever, Taffy, QuickJS) with no sidecars or browser download — also embeddable as a WASI Preview 2 component.

- **Live Wasm demo:** <https://tinybrowser.ericc.ch/demo>
- **WPT conformance dashboard:** <https://tinybrowser.ericc.ch/wpt>

## Install

macOS and Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/tinybrowser-dev/tinybrowser/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/tinybrowser-dev/tinybrowser/main/install.ps1 | iex
```

Or download an archive directly from [GitHub Releases](https://github.com/tinybrowser-dev/tinybrowser/releases).

### Platform support (`v0.1.0`)

| Target | Status | Notes |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | Shipped | Built on Ubuntu 22.04 (glibc 2.35+). Requires system `libssl3` and `libfontconfig1`. |
| `aarch64-apple-darwin` | Shipped | Apple Silicon macOS (11.0+). Uses system Security.framework. |
| `x86_64-apple-darwin` | Shipped | Intel macOS (11.0+). Uses system Security.framework. |
| `x86_64-pc-windows-msvc` | Shipped | Windows x64 (`.zip`). Uses system Schannel. |
| `wasm32-wasip2` | Shipped | Embeddable page engine component (`tinybrowser.wasm`, host supplies one-hop HTTP). |
| Linux `aarch64`, Termux / Android `aarch64` | Planned | |

## Usage

`tinybrowser` runs a unified loopback daemon serving both **Chrome DevTools Protocol (CDP)** and **W3C Classic WebDriver** on the same port.

```sh
# Profile-locked ephemeral port (writes $XDG_RUNTIME_DIR/tinybrowser/<profile>/daemon.json):
tinybrowser

# Named profile:
tinybrowser --profile agent

# Explicit port:
tinybrowser --port 4444
```

When started without `--port`, the daemon acquires `$XDG_RUNTIME_DIR/tinybrowser/<profile>/lock`, binds a loopback port, and writes its endpoint to `$XDG_RUNTIME_DIR/tinybrowser/<profile>/daemon.json`:

```json
{ "pid": 12345, "host": "127.0.0.1", "port": 41234 }
```

Connect from Playwright over CDP:

```js
import { chromium } from "playwright-core";

const browser = await chromium.connectOverCDP("http://127.0.0.1:41234");
const page = browser.contexts()[0].pages()[0];
await page.goto("https://example.com");
console.log(await page.title());
await page.screenshot({ path: "example.png" });
```

Or connect any W3C WebDriver client to `http://127.0.0.1:<port>` (`GET /status`, `POST /session`).

Optional network and logging flags: `--port <PORT>`, `--profile <NAME>`, `--proxy <URI>`, `--resolve <PATTERN=ADDR>`, `--tls-ca <PATH>`, `--log-level <LEVEL>`, `--log-file <PATH>`, `--verbose`. Run `tinybrowser --help` for full endpoint and path documentation.

### Embedded Wasm component

The `wasm32-wasip2` release tarball ships `tinybrowser.wasm` implementing [crates/wasm/wit/browser.wit](crates/wasm/wit/browser.wit). The component runs the same Rust fetch core, DOM, QuickJS, and Blitz paint pipeline as the native binary; the host supplies one raw HTTP exchange at a time (`start-fetch` / `cancel-fetch`). See [website/src/demo/](website/src/demo/) for a working browser host.

## What works / what doesn't yet

**Works:**
- HTML5 parsing (html5ever), CSS cascade & layout (Stylo + Taffy flexbox/grid), one-shot PNG/RGBA screenshots (`blitz-paint` + `anyrender-tiny-skia`)
- JavaScript via QuickJS (`quickjs-ng` fork), WebIDL bindings, DOM, events, `fetch`, XHR, Streams, URL, Encoding, Forms, Custom Elements, Canvas 2D, `Intl` (`NumberFormat` / `DateTimeFormat` via ICU4X)
- Dedicated and Shared Workers, `IndexedDB`, `CookieStore`, `localStorage` / `sessionStorage`, `postMessage` / `MessageChannel` / `BroadcastChannel`
- Browser-owned HTTP/1.1 + HTTP/2 network stack (`crates/net`): CORS, ORB, 6265bis cookies + CHIPS, CSP, HTTP cache, `Sec-Fetch-*`
- CDP (Playwright-compatible) and classic WebDriver

**Not yet (see [docs/limitations.md](docs/limitations.md)):**
- No Service Workers, WebGL, WebAudio, or page-facing WebSockets
- QuickJS is a bytecode interpreter with no JIT (JS-heavy apps run slower than V8)
- No GPU acceleration or continuous paint loop (headless one-shot rendering only)
- No OS process sandbox / OOPIF isolation yet
- OpenSSL TLS fingerprint (not Chrome JA3/JA4 impersonation)

## Building from source

Requires the pinned nightly toolchain in [rust-toolchain.toml](rust-toolchain.toml) (or Nix via `nix develop`) plus `libssl-dev`, `libfontconfig1-dev`, and `pkg-config`:

```sh
git submodule update --init --depth 1 third_party/blitz third_party/rquickjs third_party/html5ever
git -C third_party/rquickjs submodule update --init --depth 1 sys/quickjs
cargo build --release -p tinybrowser
```

Run workspace checks with `tools/check clippy` and unit tests with `cargo test --workspace`.

## License & credits

MIT licensed ([LICENSE](LICENSE)). Third-party notices live in [NOTICE](NOTICE) and project acknowledgements in [CREDITS.md](CREDITS.md).
