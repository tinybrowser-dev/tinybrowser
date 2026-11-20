/**
 * Host side of the `tinybrowser:browser/host` WIT contract.
 *
 * The Wasm component runs the real fetch core (redirects, cookies, CORS,
 * `Sec-Fetch-*`) and calls `startFetch` for one raw HTTP hop at a time. This
 * module relays each hop to `/api/hop` and delivers the result back via the
 * `completeFetch` callback registered by the page driver.
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
}

interface HostHooks {
  completeFetch: (id: bigint, outcome: FetchOutcome) => boolean
  onSettled: (record: HopRecord) => void
  onWake: () => void
  hopUrl?: string
}

const exchanges = new Map<number, AbortController>()
let inFlightCount = 0
let hopUrl = "/api/hop"
let hooks: HostHooks = {
  completeFetch: () => false,
  onSettled: () => {},
  onWake: () => {},
}

export function configure(next: Partial<HostHooks>): void {
  hooks = { ...hooks, ...next }
  if (next.hopUrl !== undefined) {
    hopUrl = next.hopUrl
  }
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

  void exchange(request, controller.signal).then((outcome) => {
    settle({
      id,
      owner,
      method: request.method,
      url: request.url,
      requestHeaders: request.headers,
      outcome,
      durationMs: Math.round(performance.now() - started),
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
        body: request.body === undefined ? null : toBase64(request.body),
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
      body: fromBase64(answer.body ?? ""),
    },
  }
}

function fromBase64(text: string): Uint8Array {
  const binary = atob(text)
  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index)
  }
  return bytes
}

function toBase64(bytes: Uint8Array): string {
  let binary = ""
  for (const byte of bytes) {
    binary += String.fromCharCode(byte)
  }
  return btoa(binary)
}
