# tinybrowser

A small, spec-first headless browser for AI agents.

One ~17 MiB Rust binary (Blitz, Stylo, html5ever, Taffy, QuickJS) with no sidecars or browser download — also embeddable as a WASI Preview 2 component.

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

### Platform support

| Target | Status | Notes |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | Shipped | Fully static binary (musl + vendored OpenSSL, pure-Rust font discovery). Zero runtime shared library dependencies; works on Alpine and any Linux x64 distro. |
| `aarch64-apple-darwin` | Shipped | Apple Silicon macOS (11.0+). Uses system Security.framework. |
| `x86_64-pc-windows-msvc` | Shipped | Windows x64 (`.zip`). Uses system Schannel. |
| `aarch64-linux-android` | Shipped | Android / Termux arm64 (API 24+, bionic). Vendored OpenSSL, no extra runtime packages required. |
| `wasm32-wasip2` | Shipped | Embeddable page engine component (`tinybrowser.wasm`, host supplies one-hop HTTP). |
| Linux `aarch64` | Planned | |

## Usage

`tinybrowser` runs a profile-locked loopback daemon serving both **Chrome DevTools Protocol (CDP)** and **W3C WebDriver** on the same port. Start it explicitly once, keep it open across tabs and commands, and stop it explicitly when done:

```sh
# Start detached daemon and wait until ready (prints endpoint JSON):
tinybrowser start
# -> {"profile":"default","pid":12345,"host":"127.0.0.1","port":41234,"url":"http://127.0.0.1:41234"}

# Named profile or fixed port:
tinybrowser start --profile agent
tinybrowser start --port 4444

# Inspect or stop explicitly:
tinybrowser status
tinybrowser stop

# Run in the foreground (for Docker / systemd / supervisors):
tinybrowser serve --port 4444
```

`tinybrowser start` acquires `$XDG_RUNTIME_DIR/tinybrowser/<profile>/lock`, binds a loopback port, verifies a renderer starts cleanly, writes `$XDG_RUNTIME_DIR/tinybrowser/<profile>/daemon.json`, and prints the endpoint as single-line JSON. Running `start` again for the same profile reuses the running daemon.

Connect from Playwright over CDP:

```js
import { chromium } from "playwright-core";

const browser = await chromium.connectOverCDP("http://127.0.0.1:41234");
const page = browser.contexts()[0].pages()[0];
await page.goto("https://example.com");
console.log(await page.title());
await page.screenshot({ path: "example.png" });
```

Or connect any W3C WebDriver Classic (`GET /status`, `POST /session`) or BiDi (`WS /session`, `WS /session/<id>`) client to `http://127.0.0.1:<port>`.

Optional flags for `start` and `serve`: `--port <PORT>`, `--profile <NAME>`, `--user-agent <VALUE>`, `--proxy <URI>`, `--resolve <PATTERN=ADDR>`, `--tls-ca <PATH>`, `--no-sandbox`, `--log-level <LEVEL>`, `--log-file <PATH>`, `--verbose`. Run `tinybrowser --help` for full endpoint and path documentation.

### Embedded Wasm component

The `wasm32-wasip2` release tarball ships `tinybrowser.wasm` implementing [crates/wasm/wit/browser.wit](crates/wasm/wit/browser.wit). The component runs the same Rust fetch core, DOM, QuickJS, and Blitz paint pipeline as the native binary; the host supplies one raw HTTP exchange at a time (`start-fetch` / `cancel-fetch`). See [website/src/demo/](website/src/demo/) for a working browser host.

## What works / what doesn't yet

**Works:**
- HTML5 parsing (html5ever), CSS cascade & layout (Stylo + Taffy flexbox/grid), one-shot PNG/RGBA screenshots (`blitz-paint` + `anyrender-tiny-skia`)
- JavaScript via QuickJS (`quickjs-ng` fork), WebIDL bindings, DOM, events, `fetch`, XHR, Streams, URL, Encoding, Forms, Custom Elements, Canvas 2D, `Intl` (`NumberFormat` / `DateTimeFormat` via ICU4X)
- Dedicated and Shared Workers, `IndexedDB`, `CookieStore`, `localStorage` / `sessionStorage`, `postMessage` / `MessageChannel` / `BroadcastChannel`
- Browser-owned HTTP/1.1 + HTTP/2 network stack (`crates/net`): CORS, ORB, 6265bis cookies + CHIPS, CSP, HTTP cache, `Sec-Fetch-*`
- Site-locked renderer processes with OS sandboxing (`Landlock` + `seccomp-bpf` on Linux, Seatbelt on macOS, Job Objects on Windows; `--no-sandbox` opt-out for containers without Landlock)
- CDP (Playwright-compatible), W3C WebDriver Classic, and W3C WebDriver BiDi

**Not yet (see [open issues](https://github.com/tinybrowser-dev/tinybrowser/issues)):**
- No Service Workers, WebGL, WebAudio, or page-facing WebSockets
- QuickJS is a bytecode interpreter with no JIT (JS-heavy apps run slower than V8)
- No GPU acceleration or continuous paint loop (headless one-shot rendering only)
- No out-of-process iframe (OOPIF) isolation yet
- OpenSSL TLS fingerprint (not Chrome JA3/JA4 impersonation)

## Building from source

Requires the pinned nightly toolchain in [rust-toolchain.toml](rust-toolchain.toml) (or Nix via `nix develop`) plus `libssl-dev` and `pkg-config` (or `--features vendored-tls` with `OPENSSL_NO_VENDOR=0` for a self-contained TLS build):

```sh
git submodule update --init --depth 1 third_party/blitz third_party/rquickjs third_party/html5ever
git -C third_party/rquickjs submodule update --init --depth 1 sys/quickjs
cargo build --release -p tinybrowser
```

For a fully static Linux `x86_64` binary (`musl` + vendored OpenSSL), install `musl-tools`, `make`, `perl`, and `lld`, add the musl target, and build with `musl-gcc`:

```sh
rustup target add x86_64-unknown-linux-musl
CC_x86_64_unknown_linux_musl=musl-gcc \
  cargo build --release -p tinybrowser --features vendored-tls --target x86_64-unknown-linux-musl
```

Run workspace checks with `tools/check clippy` and unit tests with `cargo test --workspace`.

## License & credits

MIT licensed ([LICENSE](LICENSE)). Third-party notices live in [NOTICE](NOTICE) and project acknowledgements in [CREDITS.md](CREDITS.md).
