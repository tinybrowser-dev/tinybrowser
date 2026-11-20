// The demo's host side of the contract: one HTTP exchange at a time, moved
// through the demo server's `/hop` endpoint.
//
// The component decides everything about a request — redirects, cookies, CORS,
// `Sec-Fetch-*`, referrer — and asks for exactly one exchange. This module must
// therefore move the bytes it is given and report what came back, without
// adding a header, following a redirect, or holding a cookie.

// `generated/tinybrowser.js` imports this module, so the cycle is expected:
// the engine is only read from callbacks that run after the component has
// finished instantiating.
import { engine } from "./generated/tinybrowser.js";

/// The exchanges the component is still waiting on, by request id.
const exchanges = new Map();

/// The number of entries in `exchanges`, kept separately because the page asks
/// for it after every step and a map size is not a public count.
let inFlightCount = 0;

/// The endpoint that performs one exchange, and what the page reports about it.
let hooks = {
  onSettled: () => {},
  onWake: () => {},
};

/// Where this demo performs exchanges. The server is same-origin in the page,
/// and the smoke test outside a browser passes the absolute URL itself.
let hopUrl = "/hop";

/// Points the host's reporting at the page's own sinks.
export function configure(next) {
  hooks = { ...hooks, ...next };
  hopUrl = new URL(next.hopUrl ?? hopUrl, globalThis.location?.href ?? "http://127.0.0.1:8787/")
    .href;
}

/// Performs one exchange and answers the component.
///
/// The answer may not arrive inside this call, and it does not: the request
/// goes out on a microtask, so the component's export returns first.
export function startFetch(request) {
  const id = Number(request.id);
  const controller = new AbortController();
  exchanges.set(id, controller);
  inFlightCount += 1;
  queueMicrotask(async () => {
    const outcome = await exchange(request, controller.signal);
    settle(id, outcome);
  });
  return true;
}

/// How many exchanges the component is still waiting on.
export function inFlight() {
  return inFlightCount;
}

/// Stops an exchange the component no longer wants.
export function cancelFetch(id) {
  const key = Number(id);
  const controller = exchanges.get(key);
  if (controller === undefined) {
    return;
  }
  exchanges.delete(key);
  inFlightCount -= 1;
  controller.abort();
}

function settle(id, outcome) {
  if (!exchanges.delete(id)) {
    return;
  }
  inFlightCount -= 1;
  hooks.onSettled({ id, outcome });
  engine.completeFetch(BigInt(id), outcome);
  hooks.onWake();
}

async function exchange(request, signal) {
  let answer;
  try {
    const response = await fetch(hopUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        method: request.method,
        url: request.url,
        // Header values are isomorphic strings; the server turns each code
        // point back into its wire byte.
        headers: request.headers.map(({ name, value }) => [name, value]),
        body: request.body === undefined ? null : toBase64(request.body),
        timeoutMs: request.timeoutMs === undefined ? null : Number(request.timeoutMs),
        maxBodyBytes: Number(request.maxBodyBytes),
      }),
      signal,
    });
    answer = await response.json();
  } catch {
    return { tag: "err", val: "connect" };
  }
  if (answer.error !== undefined) {
    return { tag: "err", val: answer.error };
  }
  return {
    tag: "ok",
    val: {
      status: answer.status,
      http2: answer.http2,
      headers: answer.headers.map(([name, value]) => ({ name, value })),
      body: fromBase64(answer.body),
    },
  };
}

function fromBase64(text) {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function toBase64(bytes) {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}
