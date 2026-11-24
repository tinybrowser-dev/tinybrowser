import type { ChaosConfig, HopRecord, HopStart } from "./host"
import { drawPaintDiffOverlay } from "./overlays"

export interface RenderedFrame {
  width: number
  height: number
  rgba: Uint8Array
}

export interface FrameEvent {
  frame: bigint
  event: { tag: string; val?: unknown }
}

export interface PointerEventPayload {
  x: number
  y: number
  button: number
}

export interface WheelEventPayload {
  x: number
  y: number
  deltaX: number
  deltaY: number
}

export type InputEvent =
  | { tag: "pointer-move"; val: PointerEventPayload }
  | { tag: "pointer-down"; val: PointerEventPayload }
  | { tag: "pointer-up"; val: PointerEventPayload }
  | { tag: "wheel"; val: WheelEventPayload }
  | { tag: "key-down"; val: string }
  | { tag: "key-up"; val: string }
  | { tag: "insert-text"; val: string }

export interface WasmCookie {
  name: string
  value: string
  domain: string
  path: string
  hostOnly: boolean
  secure: boolean
  httpOnly: boolean
  sameSite: string
}

export interface WasmStorageEntry {
  key: string
  value: string
}

export interface WasmTab {
  mount(url: string, html: string): void
  goto(url: string): void
  back(): boolean
  forward(): boolean
  canGoBack(): boolean
  canGoForward(): boolean
  owner(): bigint
  url(): string
  eval(source: string): string
  evalBounded(source: string, timeoutMs: bigint): string
  dispatchInput(event: InputEvent): void
  setUserAgent(value?: string): void
  userAgent(): string
  setViewport(width: number, height: number): void
  render(width: number, height: number): RenderedFrame
  step(): bigint | undefined
  events(): Array<FrameEvent>
  title(): string
  text(): string
  sessionStorage(origin: string): Array<WasmStorageEntry>
  clearSessionStorage(origin: string): void
}

export interface WasmContext {
  userAgent(): string
  openTab(url: string, html: string): WasmTab
  cookies(): Array<WasmCookie>
  setCookie(url: string, header: string): boolean
  clearCookies(): void
  localStorage(origin: string): Array<WasmStorageEntry>
  clearLocalStorage(origin: string): void
}

export interface WasmEngine {
  Context: new (userAgent?: string) => WasmContext
  Tab: new (url: string, html: string) => WasmTab
  registerFont(data: Uint8Array): number
  completeFetch(id: bigint, outcome: unknown): boolean
}

export interface HostModule {
  configure(hooks: {
    completeFetch?: (id: bigint, outcome: unknown) => boolean
    onStart?: (request: HopStart) => void
    onSettled?: (record: HopRecord) => void
    onWake?: () => void
  }): void
  setChaos(next: Partial<ChaosConfig>): ChaosConfig
  getChaos(): ChaosConfig
  inFlight(): number
}

export type StageMode = "single" | "dual" | "split"
export type SessionSlot = "a" | "b"
export type InspectorTab = "console" | "network" | "storage" | "agent" | "source" | "lab"
export type UserAgentPreset = "tinybrowser" | "chrome"

export interface SessionState {
  slot: SessionSlot
  label: string
  context: WasmContext
  tab: WasmTab
  lastRgba: Uint8Array | null
  lastSource: { kind: "url"; url: string } | { kind: "html"; url: string; html: string }
}

export interface HopEntry {
  id: number
  owner: number
  method: string
  url: string
  record?: HopRecord
}

export const VIEWPORT_HEIGHT = 400
export const DUAL_MOBILE_WIDTH = 375

export const WEBDRIVER_KEYS: Record<string, string> = {
  Backspace: "\uE003",
  Tab: "\uE004",
  Enter: "\uE006",
  Shift: "\uE008",
  Control: "\uE009",
  Alt: "\uE00A",
  Escape: "\uE00C",
  " ": "\uE00D",
  PageUp: "\uE00E",
  PageDown: "\uE00F",
  End: "\uE010",
  Home: "\uE011",
  ArrowLeft: "\uE012",
  ArrowUp: "\uE013",
  ArrowRight: "\uE014",
  ArrowDown: "\uE015",
  Delete: "\uE017",
}

export const HIGHLIGHT_SCRIPT = `(() => {
  const id = '__tb_hl';
  const existing = document.getElementById(id);
  if (existing) { existing.remove(); return 'highlights removed'; }
  const s = document.createElement('style');
  s.id = id;
  s.textContent = 'h1,h2,h3,a { outline: 2px solid #111 !important; background: #e8e8e8 !important; color: #111 !important; }';
  (document.head || document.documentElement).appendChild(s);
  return 'highlighted ' + document.querySelectorAll('h1,h2,h3,a').length + ' headings and links';
})()`

export const DARK_CSS_SCRIPT = `(() => {
  const id = '__tb_inv';
  const existing = document.getElementById(id);
  if (existing) { existing.remove(); return 'restored original colors'; }
  const s = document.createElement('style');
  s.id = id;
  s.textContent = 'html, body, table, td, tr, div { background: #111111 !important; color: #ededed !important; border-color: #333333 !important; } a { color: #fcfcfc !important; }';
  (document.head || document.documentElement).appendChild(s);
  return 'injected monochrome dark stylesheet';
})()`

export const LINKS_SCRIPT = `Array.from(document.querySelectorAll('a[href]')).slice(0, 12).map((a, i) => (i + 1) + '. ' + (a.textContent || '').trim().slice(0, 50) + ' -> ' + a.href).join('\\n') || '(no links)'`

export function formatHopSummary(entry: HopEntry): string {
  if (!entry.record) {
    return `#${entry.id} ${entry.method} ${entry.url} -> pending…`
  }
  const rec = entry.record
  const outcomeText =
    rec.outcome.tag === "ok"
      ? `${rec.outcome.val.status} (${rec.outcome.val.body.byteLength} B)`
      : `ERR ${rec.outcome.val}`
  const tags: Array<string> = []
  if (rec.chaosNote) tags.push(`chaos:${rec.chaosNote}`)
  if (rec.requestHeaders.some((h) => h.name.toLowerCase() === "cookie")) tags.push("Cookie")
  if (
    rec.outcome.tag === "ok" &&
    rec.outcome.val.headers.some((h) => h.name.toLowerCase() === "set-cookie")
  ) {
    tags.push("Set-Cookie")
  }
  if (rec.outcome.tag === "ok" && rec.outcome.val.status >= 300 && rec.outcome.val.status < 400) {
    const loc = rec.outcome.val.headers.find((h) => h.name.toLowerCase() === "location")?.value
    if (loc) tags.push(`-> ${loc}`)
  }
  const suffix = tags.length > 0 ? ` [${tags.join(", ")}]` : ""
  return `#${rec.id} ${rec.method} ${rec.url} -> ${outcomeText} · ${rec.durationMs}ms${suffix}`
}

export function formatHopDetail(rec: HopRecord): string {
  const reqLines = rec.requestHeaders.map((h) => `> ${h.name}: ${h.value}`)
  const resLines =
    rec.outcome.tag === "ok"
      ? rec.outcome.val.headers.map((h) => `< ${h.name}: ${h.value}`)
      : [`< error: ${rec.outcome.val}`]
  return [`tab owner: #${rec.owner}`, ...reqLines, "", ...resLines].join("\n")
}

export function clearOverlayCanvas(target: HTMLCanvasElement): void {
  const ctx = target.getContext("2d")
  if (ctx) ctx.clearRect(0, 0, target.width, target.height)
}

export function paintCanvas(options: {
  session: SessionState
  targetCanvas: HTMLCanvasElement
  targetOverlay: HTMLCanvasElement
  width: number
  height: number
  isPrimary: boolean
  paintDiffEnabled: boolean
  onClearDiff: () => void
}): { meta: string; diffTimer?: ReturnType<typeof setTimeout> } {
  const {
    session,
    targetCanvas,
    targetOverlay,
    width,
    height,
    isPrimary,
    paintDiffEnabled,
    onClearDiff,
  } = options
  const ctx = targetCanvas.getContext("2d")
  if (!ctx) return { meta: "" }
  const started = performance.now()
  try {
    const frame = session.tab.render(width, height)
    targetCanvas.width = frame.width
    targetCanvas.height = frame.height
    targetOverlay.width = frame.width
    targetOverlay.height = frame.height

    const pixels = new Uint8ClampedArray(frame.rgba.byteLength)
    pixels.set(frame.rgba)
    ctx.putImageData(new ImageData(pixels, frame.width, frame.height), 0, 0)

    const elapsed = Math.round(performance.now() - started)
    let diffNote = ""
    let diffTimer: ReturnType<typeof setTimeout> | undefined

    if (paintDiffEnabled && session.lastRgba !== null && isPrimary) {
      const summary = drawPaintDiffOverlay(targetOverlay, session.lastRgba, frame.rgba)
      if (summary && summary.changedPixels > 0) {
        diffNote = ` · Δ ${summary.changedPixels.toLocaleString()}px (${summary.percent.toFixed(1)}%)`
        diffTimer = setTimeout(onClearDiff, 650)
      }
    }

    if (isPrimary) {
      session.lastRgba = new Uint8Array(frame.rgba)
    }

    return {
      meta: `${frame.width}×${frame.height} RGBA · ${elapsed}ms${diffNote}`,
      diffTimer,
    }
  } catch (error: unknown) {
    return { meta: `render failed: ${formatError(error)}` }
  }
}

export function canvasPoint(
  canvas: HTMLCanvasElement,
  event: MouseEvent,
): { x: number; y: number } | null {
  const rect = canvas.getBoundingClientRect()
  if (rect.width === 0 || rect.height === 0) return null
  return {
    x: ((event.clientX - rect.left) / rect.width) * canvas.width,
    y: ((event.clientY - rect.top) / rect.height) * canvas.height,
  }
}

export async function registerSiteFonts(wasmEngine: WasmEngine): Promise<number> {
  const fontUrl = document.querySelector<HTMLLinkElement>('link[rel="preload"][as="font"]')?.href
  if (!fontUrl) return 0
  try {
    const response = await fetch(fontUrl)
    if (!response.ok) return 0
    const bytes = new Uint8Array(await response.arrayBuffer())
    return wasmEngine.registerFont(bytes)
  } catch {
    return 0
  }
}

export function safeRead(fn: () => string): string {
  try {
    return fn()
  } catch (error: unknown) {
    return `<error: ${formatError(error)}>`
  }
}

export function safeEvents(tab: WasmTab): Array<FrameEvent> {
  try {
    return tab.events()
  } catch {
    return []
  }
}

export function formatError(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
