# tinybrowser in a browser

The page engine compiled to a WebAssembly component and driven by the page it
runs in. The component is the same code the native browser runs: the fetch
core, the renderer, QuickJS, and the DOM. Only the byte mover differs — the
component asks its host for one HTTP exchange at a time instead of opening a
socket.

## Build and run

```
cargo build -p tinybrowser-wasm --target wasm32-wasip2 --release
npm install
npm run build
npm start
```

Then open <http://127.0.0.1:8787/> and press Load.

`npm run build` transpiles the component into `generated/` with `jco`. The page
imports it as an ES module, and `index.html`'s import map points the component's
WASI imports at `@bytecodealliance/preview2-shim`.

## What the host does

`host.mjs` implements the component's two imported functions:

- `start-fetch` gets one request — method, URL, complete header list, body,
  deadline, body cap — and must answer with one response: status, header list,
  body. Redirects are not followed and cookies are not touched, because the
  component already decided both.
- `cancel-fetch` stops an exchange whose tab or dial went away.

The answer comes back through `engine.complete-fetch`, never inside
`start-fetch`: the component's export must return first.

## Why the server is in the middle

Cookies travel as headers, and the component owns the jar. A browser `fetch`
cannot set `Cookie`, cannot read `Set-Cookie`, and follows redirects on its own,
so it cannot be the host. `serve.mjs` provides the intermediary the contract
expects: `POST /hop` performs exactly one exchange over `node:http` and reports
the status, the header list verbatim (repeated fields included), and the raw
body. That is also why a 3xx reaches the component as a status rather than
being chased by the server.

The server is a development stand-in for a deployable one; it trusts its caller
and reaches any URL the component names. Do not expose it.
