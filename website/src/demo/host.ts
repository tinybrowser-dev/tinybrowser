/**
 * Host side of the `tinybrowser:browser/host` WIT contract.
 *
 * The Wasm component runs the real fetch core (redirects, cookies, CORS,
 * `Sec-Fetch-*`) and calls `startFetch` for one raw HTTP hop at a time. This
 * module relays each hop to `/api/hop`, applies any active Lab chaos knobs
 * below the engine's fetch stack, and delivers the result back via
 * `completeFetch`.
 */

export interface FetchHeader {
  name: string
  value: string
}

export interface FetchRequest {
  id: bigint
  owner: bigint
  method: string
  url: string
  headers: Array<FetchHeader>
  body?: Uint8Array
  timeoutMs?: bigint
  maxBodyBytes: bigint
}

export type FetchOutcome =
  | {
      tag: "ok"
      val: {
        status: number
        http2: boolean
        headers: Array<FetchHeader>
        body: Uint8Array
      }
    }
  | { tag: "err"; val: string }

export interface HopRecord {
  id: number
  owner: number
  method: string
  url: string
  requestHeaders: Array<FetchHeader>
  outcome: FetchOutcome
  durationMs: number
  chaosNote?: string
}

export interface HopStart {
  id: number
  owner: number
  method: string
  url: string
}

export interface ChaosConfig {
  offline: boolean
  latencyMs: number
  blockCss: boolean
  blockJs: boolean
  blockImages: boolean
  stripCors: boolean
  force500: boolean
}

interface HostHooks {
  completeFetch: (id: bigint, outcome: FetchOutcome) => boolean
  onStart: (request: HopStart) => void
  onSettled: (record: HopRecord) => void
  onWake: () => void
  hopUrl?: string
}

const exchanges = new Map<number, AbortController>()
let inFlightCount = 0
let hopUrl = "/api/hop"
let chaos: ChaosConfig = {
  offline: false,
  latencyMs: 0,
  blockCss: false,
  blockJs: false,
  blockImages: false,
  stripCors: false,
  force500: false,
}
let hooks: HostHooks = {
  completeFetch: () => false,
  onStart: () => {},
  onSettled: () => {},
  onWake: () => {},
}

export function configure(next: Partial<HostHooks>): void {
  hooks = { ...hooks, ...next }
  if (next.hopUrl !== undefined) {
    hopUrl = next.hopUrl
  }
}

export function setChaos(next: Partial<ChaosConfig>): ChaosConfig {
  chaos = { ...chaos, ...next }
  return { ...chaos }
}

export function getChaos(): ChaosConfig {
  return { ...chaos }
}

export function inFlight(): number {
  return inFlightCount
}

export function startFetch(request: FetchRequest): boolean {
  const id = Number(request.id)
  const owner = Number(request.owner)
  const started = performance.now()
  const controller = new AbortController()
  exchanges.set(id, controller)
  inFlightCount += 1
  hooks.onStart({ id, owner, method: request.method, url: request.url })

  void exchangeWithChaos(request, controller.signal).then(({ outcome, chaosNote }) => {
    settle({
      id,
      owner,
      method: request.method,
      url: request.url,
      requestHeaders: request.headers,
      outcome,
      durationMs: Math.round(performance.now() - started),
      chaosNote,
    })
  })

  return true
}

export function cancelFetch(id: bigint): void {
  const key = Number(id)
  const controller = exchanges.get(key)
  if (controller === undefined) return
  exchanges.delete(key)
  inFlightCount -= 1
  controller.abort()
}

function settle(record: HopRecord): void {
  if (!exchanges.delete(record.id)) return
  inFlightCount -= 1
  hooks.onSettled(record)
  hooks.completeFetch(BigInt(record.id), record.outcome)
  hooks.onWake()
}

interface HopAnswer {
  error?: string
  status?: number
  http2?: boolean
  headers?: Array<[string, string]>
  body?: string
}

function headerValue(headers: Array<FetchHeader>, target: string): string {
  for (const header of headers) {
    if (header.name.toLowerCase() === target) return header.value.toLowerCase()
  }
  return ""
}

function classifyBlockedSubresource(request: FetchRequest): string | null {
  const dest = headerValue(request.headers, "sec-fetch-dest")
  const pathname = URL.parse(request.url)?.pathname.toLowerCase() ?? ""
  if (chaos.blockCss && (dest === "style" || pathname.endsWith(".css"))) {
    return "blocked CSS"
  }
  if (
    chaos.blockJs &&
    (dest === "script" || pathname.endsWith(".js") || pathname.endsWith(".mjs"))
  ) {
    return "blocked JS"
  }
  if (
    chaos.blockImages &&
    (dest === "image" || /\.(png|jpe?g|gif|webp|svg|ico|avif)$/i.test(pathname))
  ) {
    return "blocked image"
  }
  return null
}

function delay(ms: number, signal: AbortSignal): Promise<void> {
  if (ms <= 0) return Promise.resolve()
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort)
      resolve()
    }, ms)
    function onAbort(): void {
      clearTimeout(timer)
      resolve()
    }
    signal.addEventListener("abort", onAbort, { once: true })
  })
}

async function exchangeWithChaos(
  request: FetchRequest,
  signal: AbortSignal,
): Promise<{ outcome: FetchOutcome; chaosNote?: string }> {
  if (chaos.latencyMs > 0) {
    await delay(chaos.latencyMs, signal)
    if (signal.aborted) {
      return { outcome: { tag: "err", val: "cancelled" } }
    }
  }

  if (chaos.offline) {
    return { outcome: { tag: "err", val: "connect" }, chaosNote: "offline" }
  }

  const blocked = classifyBlockedSubresource(request)
  if (blocked !== null) {
    return { outcome: { tag: "err", val: "connect" }, chaosNote: blocked }
  }

  if (chaos.force500) {
    return {
      outcome: {
        tag: "ok",
        val: {
          status: 500,
          http2: true,
          headers: [{ name: "content-type", value: "text/plain; charset=utf-8" }],
          body: new TextEncoder().encode("500 injected by tinybrowser lab chaos mode"),
        },
      },
      chaosNote: "forced 500",
    }
  }

  const outcome = await exchange(request, signal)
  if (chaos.stripCors && outcome.tag === "ok") {
    const filtered = outcome.val.headers.filter(
      (header) => !header.name.toLowerCase().startsWith("access-control-"),
    )
    const stripped = filtered.length !== outcome.val.headers.length
    return {
      outcome: {
        tag: "ok",
        val: { ...outcome.val, headers: filtered },
      },
      chaosNote: stripped ? "stripped CORS" : undefined,
    }
  }

  return { outcome }
}

async function exchange(request: FetchRequest, signal: AbortSignal): Promise<FetchOutcome> {
  let answer: HopAnswer
  try {
    const response = await fetch(hopUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        method: request.method,
        url: request.url,
        headers: request.headers.map(({ name, value }) => [name, value]),
        body: request.body === undefined ? null : request.body.toBase64(),
        timeoutMs: request.timeoutMs === undefined ? null : Number(request.timeoutMs),
        maxBodyBytes: Number(request.maxBodyBytes),
      }),
      signal,
    })
    if (!response.ok) {
      return { tag: "err", val: "connect" }
    }
    answer = (await response.json()) as HopAnswer
  } catch {
    return { tag: "err", val: "connect" }
  }

  if (answer.error !== undefined) {
    return { tag: "err", val: answer.error }
  }

  return {
    tag: "ok",
    val: {
      status: answer.status ?? 200,
      http2: Boolean(answer.http2),
      headers: (answer.headers ?? []).map(([name, value]) => ({ name, value })),
      body: Uint8Array.fromBase64(answer.body ?? ""),
    },
  }
}
