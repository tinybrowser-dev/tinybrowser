# Changelog

All notable changes to `tinybrowser` are documented in this file.

## [Unreleased]

## [0.0.1] - 2026-11-22

### Added
- Single-binary headless browser built on Blitz, Stylo, html5ever, Taffy, and QuickJS (`quickjs-ng` fork)
- Unified loopback daemon serving Chrome DevTools Protocol (CDP) and W3C Classic WebDriver
- Browser-owned HTTP/1.1 + HTTP/2 network stack (`crates/net`) with CORS, ORB, 6265bis cookies + CHIPS, CSP, HTTP cache, and `Sec-Fetch-*`
- Dedicated and Shared Workers, `IndexedDB`, `CookieStore`, `localStorage`/`sessionStorage`, `postMessage`/`MessageChannel`/`BroadcastChannel`, Canvas 2D, `Intl` (ICU4X)
- Embeddable `wasm32-wasip2` component (`tinybrowser.wasm`) and live browser demo
- Fully static Linux `x86_64-unknown-linux-musl` release binary (vendored OpenSSL + pure-Rust font discovery) alongside macOS, Windows, and Android/Termux builds
- Upstream Playwright (`v1.63.0`) compatibility harness (`tools/playwright/upstream-run.mjs`)
