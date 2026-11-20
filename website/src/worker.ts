/**
 * Cloudflare Worker backing tinybrowser.ericc.ch/api/*.
 *
 * Two surfaces live here:
 * - `/api/demo/*`: self-contained fixtures (302 redirect, Set-Cookie script,
 *   cookie echo) so the Wasm demo proves redirects and cookies without needing
 *   an external origin.
 * - `POST /api/hop`: one raw HTTP exchange for the Wasm fetch core. Browser
 *   `fetch` cannot set `Cookie`, read `Set-Cookie`, or return raw 3xx hops, so
 *   this Worker moves the bytes for a single hop while the Rust fetch core
 *   inside Wasm owns redirects, cookies, CORS, and `Sec-Fetch-*`.
 */

interface Fetcher {
  fetch(request: Request): Promise<Response>
}

interface Env {
  ASSETS?: Fetcher
}

interface HopRequest {
  method: string
  url: string
  headers: Array<[string, string]>
  body: string | null
  timeoutMs: number | null
  maxBodyBytes: number
}

type HopOutcome =
  | {
      status: number
      http2: boolean
      headers: Array<[string, string]>
      body: string
    }
  | { error: "dns" | "connect" | "tls" | "timeout" | "limit" | "queue-full" }

const ALLOWED_METHODS = new Set(["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"])
const STRIPPED_REQUEST_HEADERS = new Set([
  "host",
  "connection",
  "content-length",
  "transfer-encoding",
  "keep-alive",
  "upgrade",
])
const MAX_WORKER_BODY_BYTES = 4 * 1024 * 1024
const DEFAULT_TIMEOUT_MS = 10_000
const MAX_TIMEOUT_MS = 15_000

const worker = {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url)

    if (url.pathname === "/api/hop") {
      if (request.method !== "POST") {
        return new Response("method not allowed", { status: 405 })
      }
      return handleHop(request, url)
    }

    if (url.pathname.startsWith("/api/demo/")) {
      return handleDemoFixture(url.pathname, request.headers.get("cookie"))
    }

    if (env.ASSETS) {
      return env.ASSETS.fetch(request)
    }
    return new Response("not found", { status: 404 })
  },
}

export default worker

function handleDemoFixture(pathname: string, cookieHeader: string | null): Response {
  if (pathname === "/api/demo/redirect") {
    return new Response(null, {
      status: 302,
      headers: { location: "/api/demo/hello.txt" },
    })
  }
  if (pathname === "/api/demo/hello.js") {
    return new Response('document.body.dataset.subresource = "script loaded";\n', {
      status: 200,
      headers: {
        "content-type": "text/javascript; charset=utf-8",
        "set-cookie": "tb_demo=1; Path=/",
      },
    })
  }
  if (pathname === "/api/demo/hello.txt") {
    return new Response("hello from tinybrowser demo origin\n", {
      status: 200,
      headers: { "content-type": "text/plain; charset=utf-8" },
    })
  }
  if (pathname === "/api/demo/echo") {
    return new Response(`cookie: ${cookieHeader ?? "(none)"}\n`, {
      status: 200,
      headers: { "content-type": "text/plain; charset=utf-8" },
    })
  }
  return new Response("not found", { status: 404 })
}

async function handleHop(request: Request, requestUrl: URL): Promise<Response> {
  if (request.headers.get("origin") !== requestUrl.origin) {
    return new Response("forbidden", { status: 403 })
  }
  const contentType = request.headers.get("content-type") ?? ""
  const mediaType = contentType.split(";", 1)[0]?.trim().toLowerCase() ?? ""
  if (mediaType !== "application/json") {
    return new Response("unsupported media type", { status: 415 })
  }

  let hop: HopRequest
  try {
    hop = (await request.json()) as HopRequest
  } catch {
    return new Response("bad hop request", { status: 400 })
  }

  const method = typeof hop.method === "string" ? hop.method.toUpperCase() : ""
  if (!ALLOWED_METHODS.has(method)) {
    return json({ error: "connect" })
  }

  let target: URL
  try {
    target = new URL(hop.url)
  } catch {
    return json({ error: "connect" })
  }

  if (target.protocol !== "http:" && target.protocol !== "https:") {
    return json({ error: "connect" })
  }

  // Serve same-origin `/api/demo/*` fixtures directly in-worker so the demo
  // never triggers a Workers self-fetch loop.
  if (target.origin === requestUrl.origin && target.pathname.startsWith("/api/demo/")) {
    const cookie = findHeader(hop.headers, "cookie")
    const fixture = handleDemoFixture(target.pathname, cookie)
    return serializeResponse(fixture, MAX_WORKER_BODY_BYTES, false)
  }

  if (target.origin === requestUrl.origin || isBlockedHost(target)) {
    return json({ error: "connect" })
  }

  const headers = new Headers()
  for (const [name, value] of hop.headers) {
    const lower = name.toLowerCase()
    if (STRIPPED_REQUEST_HEADERS.has(lower) || lower.startsWith("cf-")) continue
    try {
      headers.append(name, value)
    } catch {
      // Drop malformed headers rather than failing the whole hop.
    }
  }

  const maxEncodedBodyChars = Math.ceil((MAX_WORKER_BODY_BYTES * 4) / 3) + 4
  let body: Uint8Array<ArrayBuffer> | undefined
  if (hop.body !== null && method !== "GET" && method !== "HEAD") {
    if (typeof hop.body !== "string") {
      return new Response("bad hop request", { status: 400 })
    }
    if (hop.body.length > maxEncodedBodyChars) {
      return json({ error: "limit" })
    }
    try {
      body = fromBase64(hop.body)
    } catch {
      return new Response("bad hop request", { status: 400 })
    }
    if (body.byteLength > MAX_WORKER_BODY_BYTES) {
      return json({ error: "limit" })
    }
  }
  const limit = Math.min(
    Number.isFinite(hop.maxBodyBytes) && hop.maxBodyBytes > 0
      ? hop.maxBodyBytes
      : MAX_WORKER_BODY_BYTES,
    MAX_WORKER_BODY_BYTES,
  )
  const timeoutMs = Math.min(
    hop.timeoutMs !== null && Number.isFinite(hop.timeoutMs) && hop.timeoutMs > 0
      ? hop.timeoutMs
      : DEFAULT_TIMEOUT_MS,
    MAX_TIMEOUT_MS,
  )

  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), timeoutMs)
  try {
    const upstream = await fetch(target.href, {
      method,
      headers,
      body,
      redirect: "manual",
      signal: controller.signal,
    })
    return await serializeResponse(upstream, limit, true)
  } catch (error: unknown) {
    if (controller.signal.aborted) {
      return json({ error: "timeout" })
    }
    return json({ error: classifyError(error) })
  } finally {
    clearTimeout(timer)
  }
}

async function serializeResponse(
  upstream: Response,
  limit: number,
  stripEncoding: boolean,
): Promise<Response> {
  const bytes = await readBounded(upstream.body, limit)
  if (bytes === null) {
    return json({ error: "limit" })
  }

  const headers: Array<[string, string]> = []
  for (const [name, value] of upstream.headers.entries()) {
    const lower = name.toLowerCase()
    if (lower === "set-cookie") continue
    // Workers `fetch` transparently decompresses gzip/br bodies, so forwarding
    // the original `content-encoding` or `content-length` would make the
    // component's BodyReader try to decompress plain text.
    if (stripEncoding && (lower === "content-encoding" || lower === "content-length")) continue
    headers.push([name, value])
  }
  const setCookies =
    typeof upstream.headers.getSetCookie === "function" ? upstream.headers.getSetCookie() : []
  for (const cookie of setCookies) {
    headers.push(["set-cookie", cookie])
  }

  return json({
    status: upstream.status,
    http2: true,
    headers,
    body: toBase64(bytes),
  })
}

async function readBounded(
  stream: ReadableStream<Uint8Array> | null,
  limit: number,
): Promise<Uint8Array | null> {
  if (stream === null) return new Uint8Array(0)
  const reader = stream.getReader()
  const chunks: Array<Uint8Array> = []
  let total = 0
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      if (value === undefined) continue
      total += value.byteLength
      if (total > limit) {
        await reader.cancel()
        return null
      }
      chunks.push(value)
    }
  } finally {
    reader.releaseLock()
  }
  const out = new Uint8Array(total)
  let offset = 0
  for (const chunk of chunks) {
    out.set(chunk, offset)
    offset += chunk.byteLength
  }
  return out
}

function isBlockedHost(target: URL): boolean {
  if (target.port !== "" && target.port !== "80" && target.port !== "443") {
    return true
  }
  const host = target.hostname.toLowerCase()
  if (
    host === "localhost" ||
    host.endsWith(".localhost") ||
    host.endsWith(".local") ||
    host.endsWith(".internal") ||
    host.endsWith(".home.arpa")
  ) {
    return true
  }

  // IPv6 literal in URL.hostname is bracketed: `[::1]`.
  if (host.startsWith("[")) {
    const inner = host.slice(1, -1)
    if (
      inner === "::1" ||
      inner === "::" ||
      inner.startsWith("fc") ||
      inner.startsWith("fd") ||
      inner.startsWith("fe8") ||
      inner.startsWith("fe9") ||
      inner.startsWith("fea") ||
      inner.startsWith("feb") ||
      inner.startsWith("ff") ||
      inner.startsWith("::ffff:")
    ) {
      return true
    }
    return false
  }

  const ipv4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host)
  if (ipv4) {
    const a = Number(ipv4[1])
    const b = Number(ipv4[2])
    const c = Number(ipv4[3])
    const d = Number(ipv4[4])
    if (a > 255 || b > 255 || c > 255 || d > 255) return true
    if (a === 0 || a === 10 || a === 127 || a >= 224) return true
    if (a === 169 && b === 254) return true
    if (a === 172 && b >= 16 && b <= 31) return true
    if (a === 192 && b === 168) return true
    if (a === 100 && b >= 64 && b <= 127) return true
    if (a === 192 && b === 0 && (c === 0 || c === 2)) return true
    if (a === 198 && (b === 18 || b === 19)) return true
    if (a === 198 && b === 51 && c === 100) return true
    if (a === 203 && b === 0 && c === 113) return true
  }

  return false
}

function findHeader(headers: Array<[string, string]>, target: string): string | null {
  const values: Array<string> = []
  for (const [name, value] of headers) {
    if (name.toLowerCase() === target) values.push(value)
  }
  return values.length > 0 ? values.join("; ") : null
}

function classifyError(error: unknown): "dns" | "tls" | "connect" {
  const text = String(error instanceof Error ? error.message : error).toLowerCase()
  if (text.includes("dns") || text.includes("resolve") || text.includes("not found")) return "dns"
  if (text.includes("tls") || text.includes("ssl") || text.includes("certificate")) return "tls"
  return "connect"
}

function json(outcome: HopOutcome): Response {
  return new Response(JSON.stringify(outcome), {
    status: 200,
    headers: { "content-type": "application/json" },
  })
}

function fromBase64(text: string): Uint8Array<ArrayBuffer> {
  const binary = atob(text)
  const bytes = new Uint8Array(new ArrayBuffer(binary.length))
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index)
  }
  return bytes
}

function toBase64(bytes: Uint8Array): string {
  const CHUNK = 0x8000
  const parts: Array<string> = []
  for (let offset = 0; offset < bytes.length; offset += CHUNK) {
    const slice = bytes.subarray(offset, offset + CHUNK)
    let binary = ""
    for (const byte of slice) {
      binary += String.fromCharCode(byte)
    }
    parts.push(binary)
  }
  return btoa(parts.join(""))
}
