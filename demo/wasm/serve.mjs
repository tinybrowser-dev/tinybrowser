// The demo's server: static files for the page, and the hop endpoint that
// performs one HTTP exchange for the component.
//
// The component owns every fetch rule — redirects, cookies, CORS, `Sec-Fetch-*`
// — and asks for one exchange at a time. A browser `fetch` cannot do that: it
// follows redirects, refuses to set `Cookie`, and hides `Set-Cookie`. This
// server is the intermediary the host contract expects, so the demo exercises
// the same fetch core the native browser runs.

import { createServer } from "node:http";
import { request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";
import { readFileSync, existsSync } from "node:fs";
import { dirname, extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const port = Number(process.env.PORT ?? 8787);

const types = new Map([
  [".html", "text/html; charset=utf-8"],
  [".mjs", "text/javascript; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".wasm", "application/wasm"],
  [".json", "application/json"],
  [".css", "text/css; charset=utf-8"],
]);

createServer((request, response) => {
  if (request.url === "/hop" && request.method === "POST") {
    hop(request, response);
    return;
  }
  if (request.url === "/redirect") {
    response.writeHead(302, { location: "/hello.txt" });
    response.end();
    return;
  }
  if (request.url === "/echo") {
    response.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
    response.end(`cookie: ${request.headers.cookie ?? "(none)"}\n`);
    return;
  }
  if (request.url === "/hello.js" || request.url === "/hello.txt") {
    serveHello(request.url, response);
    return;
  }
  serveStatic(request, response);
}).listen(port, "127.0.0.1", () => {
  console.log(`tinybrowser wasm demo: http://127.0.0.1:${port}/`);
});

function serveStatic(request, response) {
  const url = new URL(request.url, `http://127.0.0.1:${port}`);
  const relative = normalize(decodeURIComponent(url.pathname)).replace(/^(\.\.[/\\])+/, "");
  const path = join(here, relative === "/" ? "index.html" : relative);
  if (!path.startsWith(here) || !existsSync(path)) {
    response.writeHead(404).end("not found");
    return;
  }
  response.writeHead(200, {
    "content-type": types.get(extname(path)) ?? "application/octet-stream",
  });
  response.end(readFileSync(path));
}

function serveHello(path, response) {
  if (path.endsWith(".js")) {
    response.writeHead(200, {
      "content-type": "text/javascript; charset=utf-8",
      "set-cookie": "hello=1; Path=/",
    });
    response.end('document.body.dataset.subresource = "script loaded";\n');
    return;
  }
  response.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
  response.end("hello from the demo origin\n");
}

/// One HTTP exchange for the component: exactly the method, URL, headers, and
/// body it names, with the status, headers, and body that came back.
///
/// Nothing is added, removed, or followed: the component decided all of that,
/// and a 3xx here is a status to report, not a redirect to chase.
function hop(request, response) {
  const chunks = [];
  request.on("data", (chunk) => chunks.push(chunk));
  request.on("end", () => {
    let hopRequest;
    try {
      hopRequest = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    } catch {
      response.writeHead(400).end("bad hop request");
      return;
    }
    const url = new URL(hopRequest.url);
    const send = url.protocol === "https:" ? httpsRequest : httpRequest;
    // The list keeps repeated fields and their order, the way the component
    // wrote it. Header values are isomorphic: one code point per wire byte.
    const headers = hopRequest.headers.map(([name, value]) => [
      name,
      Buffer.from(
        Uint8Array.from(value, (character) => character.codePointAt(0)),
      ).toString("latin1"),
    ]);
    const body = hopRequest.body == null ? null : Buffer.from(hopRequest.body, "base64");
    const upstream = send(
      url,
      { method: hopRequest.method },
      (upstreamResponse) => {
        const limit = Number(hopRequest.maxBodyBytes);
        const received = [];
        let total = 0;
        upstreamResponse.on("data", (chunk) => {
          total += chunk.length;
          if (total > limit) {
            upstream.destroy();
            answer(response, { error: "limit" });
            return;
          }
          received.push(chunk);
        });
        upstreamResponse.on("end", () => {
          const raw = Buffer.concat(received);
          // `rawHeaders` keeps repeated fields separate and leaves each byte
          // as its own code point, which is exactly the isomorphic form.
          const headers = [];
          for (let index = 0; index < upstreamResponse.rawHeaders.length; index += 2) {
            headers.push([upstreamResponse.rawHeaders[index], upstreamResponse.rawHeaders[index + 1]]);
          }
          answer(response, {
            status: upstreamResponse.statusCode,
            http2: upstreamResponse.httpVersion === "2.0",
            headers,
            body: raw.toString("base64"),
          });
        });
      },
    );
    // `appendHeader` keeps repeated fields and their order, which the object
    // form of `headers` would collapse.
    for (const [name, value] of headers) {
      upstream.appendHeader(name, value);
    }
    if (body !== null && !upstream.hasHeader("content-length")) {
      upstream.setHeader("content-length", body.length);
    }
    upstream.on("error", (error) => {
      answer(response, { error: hopError(error) });
    });
    const timeoutMs = hopRequest.timeoutMs == null ? null : Number(hopRequest.timeoutMs);
    if (timeoutMs !== null) {
      upstream.setTimeout(timeoutMs, () => {
        upstream.destroy(new Error("timeout"));
      });
    }
    if (body !== null) {
      upstream.write(body);
    }
    upstream.end();
  });
}

function answer(response, body) {
  response.writeHead(200, { "content-type": "application/json" });
  response.end(JSON.stringify(body));
}

function hopError(error) {
  if (error.code === "ENOTFOUND" || error.code === "EAI_AGAIN") {
    return "dns";
  }
  if (message(error).includes("timeout")) {
    return "timeout";
  }
  if (error.code?.startsWith("ERR_TLS") || message(error).includes("certificate")) {
    return "tls";
  }
  return "connect";
}

function message(error) {
  return String(error?.message ?? error);
}
