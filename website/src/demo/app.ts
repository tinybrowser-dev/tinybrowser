/**
 * Interactive Browser Engine Lab driver for `/demo`.
 *
 * Drives the jco-transpiled `tinybrowser-wasm` component with isolated
 * `Context` cookie jars, real session history, dual-viewport & split-session
 * stages, DevTools / Set-of-Mark / Paint-Diff overlays, event-loop stepping,
 * and host-level network chaos knobs.
 */

import fetchPresetHtml from "./fixtures/fetch-cookies.html?raw"
import interactiveAppHtml from "./fixtures/interactive.html?raw"
import type { ChaosConfig, HopRecord, HopStart } from "./host"
import {
  type AgentMark,
  type InspectHit,
  collectAgentMarks,
  drawAgentMarksOverlay,
  drawInspectOverlay,
  drawPaintDiffOverlay,
  inspectElementAt,
} from "./overlays"

interface RenderedFrame {
  width: number
  height: number
  rgba: Uint8Array
}

interface FrameEvent {
  frame: bigint
  event: { tag: string; val?: unknown }
}

interface PointerEventPayload {
  x: number
  y: number
  button: number
}

interface WheelEventPayload {
  x: number
  y: number
  deltaX: number
  deltaY: number
}

type InputEvent =
  | { tag: "pointer-move"; val: PointerEventPayload }
  | { tag: "pointer-down"; val: PointerEventPayload }
  | { tag: "pointer-up"; val: PointerEventPayload }
  | { tag: "wheel"; val: WheelEventPayload }
  | { tag: "key-down"; val: string }
  | { tag: "key-up"; val: string }
  | { tag: "insert-text"; val: string }

interface WasmCookie {
  name: string
  value: string
  domain: string
  path: string
  hostOnly: boolean
  secure: boolean
  httpOnly: boolean
  sameSite: string
}

interface WasmStorageEntry {
  key: string
  value: string
}

interface WasmTab {
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

interface WasmContext {
  userAgent(): string
  openTab(url: string, html: string): WasmTab
  cookies(): Array<WasmCookie>
  setCookie(url: string, header: string): boolean
  clearCookies(): void
  localStorage(origin: string): Array<WasmStorageEntry>
  clearLocalStorage(origin: string): void
}

interface WasmEngine {
  Context: new (userAgent?: string) => WasmContext
  Tab: new (url: string, html: string) => WasmTab
  registerFont(data: Uint8Array): number
  completeFetch(id: bigint, outcome: unknown): boolean
}

interface HostModule {
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

type StageMode = "single" | "dual" | "split"
type SessionSlot = "a" | "b"
type InspectorTab = "console" | "network" | "storage" | "agent" | "source"

interface SessionState {
  slot: SessionSlot
  label: string
  context: WasmContext
  tab: WasmTab
  lastRgba: Uint8Array | null
  lastSource: { kind: "url"; url: string } | { kind: "html"; url: string; html: string }
}

function queryRequired<T extends Element>(selector: string): T {
  const el = document.querySelector<T>(selector)
  if (!el) throw new Error(`missing element: ${selector}`)
  return el
}

const statusEl = queryRequired<HTMLElement>("#demo-status")
const navBackBtn = queryRequired<HTMLButtonElement>("#nav-back")
const navForwardBtn = queryRequired<HTMLButtonElement>("#nav-forward")
const navReloadBtn = queryRequired<HTMLButtonElement>("#nav-reload")
const urlInput = queryRequired<HTMLInputElement>("#demo-url")
const iframeBadgeEl = queryRequired<HTMLElement>("#lab-iframe-badge")
const gotoButton = queryRequired<HTMLButtonElement>("#demo-goto")

const bmHnBtn = queryRequired<HTMLButtonElement>("#bm-hn")
const bmNprBtn = queryRequired<HTMLButtonElement>("#bm-npr")
const bmExampleBtn = queryRequired<HTMLButtonElement>("#bm-example")
const bmCanvasBtn = queryRequired<HTMLButtonElement>("#bm-canvas")
const bmCookiesBtn = queryRequired<HTMLButtonElement>("#bm-cookies")

const modeSingleBtn = queryRequired<HTMLButtonElement>("#mode-single")
const modeDualBtn = queryRequired<HTMLButtonElement>("#mode-dual")
const modeSplitBtn = queryRequired<HTMLButtonElement>("#mode-split")
const stageEl = queryRequired<HTMLElement>("#lab-stage")

const loopPauseBtn = queryRequired<HTMLButtonElement>("#loop-pause")
const loopStepBtn = queryRequired<HTMLButtonElement>("#loop-step")
const uaTinybrowserBtn = queryRequired<HTMLButtonElement>("#ua-tinybrowser")
const uaChromeBtn = queryRequired<HTMLButtonElement>("#ua-chrome")

const ovInspectBtn = queryRequired<HTMLButtonElement>("#ov-inspect")
const ovMarksBtn = queryRequired<HTMLButtonElement>("#ov-marks")
const ovDiffBtn = queryRequired<HTMLButtonElement>("#ov-diff")

const vpWidthInput = queryRequired<HTMLInputElement>("#vp-width")
const vpWidthLabel = queryRequired<HTMLElement>("#vp-width-label")
const scrollUpBtn = queryRequired<HTMLButtonElement>("#scroll-up")
const scrollDownBtn = queryRequired<HTMLButtonElement>("#scroll-down")

const vpPrimaryEl = queryRequired<HTMLElement>("#vp-primary")
const vpPrimaryTitle = queryRequired<HTMLElement>("#vp-primary-title")
const frameMetaEl = queryRequired<HTMLElement>("#demo-frame-meta")
const canvasEl = queryRequired<HTMLCanvasElement>("#demo-canvas")
const overlayEl = queryRequired<HTMLCanvasElement>("#demo-overlay")

const vpSecondaryEl = queryRequired<HTMLElement>("#vp-secondary")
const vpSecondaryTitle = queryRequired<HTMLElement>("#vp-secondary-title")
const frameMetaBEl = queryRequired<HTMLElement>("#demo-frame-meta-b")
const canvasBEl = queryRequired<HTMLCanvasElement>("#demo-canvas-b")
const overlayBEl = queryRequired<HTMLCanvasElement>("#demo-overlay-b")

const hintEl = queryRequired<HTMLElement>("#demo-hint")

const tabConsoleBtn = queryRequired<HTMLButtonElement>("#tab-console")
const tabNetworkBtn = queryRequired<HTMLButtonElement>("#tab-network")
const tabNetworkCount = queryRequired<HTMLElement>("#tab-network-count")
const tabStorageBtn = queryRequired<HTMLButtonElement>("#tab-storage")
const tabAgentBtn = queryRequired<HTMLButtonElement>("#tab-agent")
const tabSourceBtn = queryRequired<HTMLButtonElement>("#tab-source")

const panelConsole = queryRequired<HTMLElement>("#panel-console")
const panelNetwork = queryRequired<HTMLElement>("#panel-network")
const panelStorage = queryRequired<HTMLElement>("#panel-storage")
const panelAgent = queryRequired<HTMLElement>("#panel-agent")
const panelSource = queryRequired<HTMLElement>("#panel-source")

const evalInput = queryRequired<HTMLInputElement>("#demo-eval")
const evalButton = queryRequired<HTMLButtonElement>("#demo-eval-run")
const actionHighlightBtn = queryRequired<HTMLButtonElement>("#action-highlight")
const actionInvertBtn = queryRequired<HTMLButtonElement>("#action-invert")
const actionLinksBtn = queryRequired<HTMLButtonElement>("#action-links")
const actionLoopBtn = queryRequired<HTMLButtonElement>("#action-loop")
const actionMultitabBtn = queryRequired<HTMLButtonElement>("#action-multitab")
const outputEl = queryRequired<HTMLElement>("#demo-output")

const chaosOfflineBtn = queryRequired<HTMLButtonElement>("#chaos-offline")
const chaosLatencyBtn = queryRequired<HTMLButtonElement>("#chaos-latency")
const chaosCssBtn = queryRequired<HTMLButtonElement>("#chaos-css")
const chaosJsBtn = queryRequired<HTMLButtonElement>("#chaos-js")
const chaosImgBtn = queryRequired<HTMLButtonElement>("#chaos-img")
const chaosCorsBtn = queryRequired<HTMLButtonElement>("#chaos-cors")
const chaos500Btn = queryRequired<HTMLButtonElement>("#chaos-500")
const hopsMetaEl = queryRequired<HTMLElement>("#demo-hops-meta")
const hopsEl = queryRequired<HTMLElement>("#demo-hops")

const storageSessionLabel = queryRequired<HTMLElement>("#storage-session-label")
const storageAddBtn = queryRequired<HTMLButtonElement>("#storage-add-httponly")
const storageClearBtn = queryRequired<HTMLButtonElement>("#storage-clear")
const storageRefreshBtn = queryRequired<HTMLButtonElement>("#storage-refresh")
const storageBodyEl = queryRequired<HTMLElement>("#storage-body")

const agentCountEl = queryRequired<HTMLElement>("#agent-count")
const agentScanBtn = queryRequired<HTMLButtonElement>("#agent-scan")
const agentMarksListEl = queryRequired<HTMLElement>("#agent-marks-list")
const agentTextEl = queryRequired<HTMLElement>("#agent-text")

const sourcePullBtn = queryRequired<HTMLButtonElement>("#source-pull")
const mountButton = queryRequired<HTMLButtonElement>("#demo-mount")
const htmlInput = queryRequired<HTMLTextAreaElement>("#demo-html")

let engine: WasmEngine | null = null
let host: HostModule | null = null
let sessionA: SessionState | null = null
let sessionB: SessionState | null = null
let activeSlot: SessionSlot = "a"
let stageMode: StageMode = "single"
let activeInspector: InspectorTab = "console"

let loopPaused = false
let inspectEnabled = false
let marksEnabled = false
let paintDiffEnabled = false
let pinnedInspect: InspectHit | null = null
let currentMarks: Array<AgentMark> = []
let hoverMarkIndex: number | null = null

let viewportWidth = 640
const viewportHeight = 400
const dualMobileWidth = 375

let wake: (() => void) | null = null
let pumpGeneration = 0
let diffTimer: ReturnType<typeof setTimeout> | null = null

interface HopEntry {
  id: number
  owner: number
  method: string
  url: string
  record?: HopRecord
}

const hopEntries = new Map<number, HopEntry>()

const WEBDRIVER_KEYS: Record<string, string> = {
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

function getActiveSession(): SessionState | null {
  return activeSlot === "b" && sessionB !== null ? sessionB : sessionA
}

function setStatus(message: string): void {
  statusEl.textContent = message
}

function setActiveSlot(slot: SessionSlot): void {
  activeSlot = slot
  vpPrimaryEl.dataset.active = slot === "a" ? "true" : "false"
  vpSecondaryEl.dataset.active = slot === "b" ? "true" : "false"
  const active = getActiveSession()
  if (active) {
    const currentUrl = safeRead(() => active.tab.url())
    if (currentUrl.startsWith("http://") || currentUrl.startsWith("https://")) {
      urlInput.value = currentUrl
    }
    syncHistoryButtons(active.tab)
    refreshStorageView()
    if (marksEnabled || activeInspector === "agent") {
      refreshAgentMarks()
    }
  }
}

function syncHistoryButtons(tab: WasmTab): void {
  navBackBtn.disabled = !tab.canGoBack()
  navForwardBtn.disabled = !tab.canGoForward()
  navReloadBtn.disabled = false
}

function clearHops(): void {
  hopEntries.clear()
  iframeBadgeEl.hidden = true
  iframeBadgeEl.textContent = ""
  renderHops()
}

function detectIframeBlockHeader(record: HopRecord): string | null {
  if (record.outcome.tag !== "ok") return null
  for (const header of record.outcome.val.headers) {
    const name = header.name.toLowerCase()
    const val = header.value.trim()
    if (name === "x-frame-options" && /deny|sameorigin/i.test(val)) {
      return `X-Frame-Options: ${val.toUpperCase()}`
    }
    if (
      name === "content-security-policy" &&
      /frame-ancestors\s+'none'|frame-ancestors\s+'self'/i.test(val)
    ) {
      return "CSP: frame-ancestors"
    }
  }
  return null
}

function recordHopStart(req: HopStart): void {
  hopEntries.set(req.id, {
    id: req.id,
    owner: req.owner,
    method: req.method,
    url: req.url,
  })
  renderHops()
}

function recordHopSettled(record: HopRecord): void {
  hopEntries.set(record.id, {
    id: record.id,
    owner: record.owner,
    method: record.method,
    url: record.url,
    record,
  })
  const blockedBadge = detectIframeBlockHeader(record)
  if (blockedBadge !== null) {
    iframeBadgeEl.textContent = `${blockedBadge} · blocked in iframes`
    iframeBadgeEl.hidden = false
  }
  renderHops()
}

function renderHops(): void {
  const count = hopEntries.size
  tabNetworkCount.textContent = String(count)
  hopsMetaEl.textContent = `${count} ${count === 1 ? "request" : "requests"} via /api/hop`
  hopsEl.replaceChildren()
  if (count === 0) {
    const empty = document.createElement("p")
    empty.className = "muted"
    empty.textContent = "No requests yet."
    hopsEl.append(empty)
    return
  }

  const entries = Array.from(hopEntries.values()).reverse()
  for (const entry of entries) {
    const details = document.createElement("details")
    details.className = "lab-hop-item"
    const summary = document.createElement("summary")

    if (!entry.record) {
      summary.textContent = `#${entry.id} ${entry.method} ${entry.url} -> pending…`
      details.append(summary)
      hopsEl.append(details)
      continue
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
    summary.textContent = `#${rec.id} ${rec.method} ${rec.url} -> ${outcomeText} · ${rec.durationMs}ms${suffix}`

    const detailPre = document.createElement("pre")
    detailPre.className = "lab-hop-detail"
    const reqLines = rec.requestHeaders.map((h) => `> ${h.name}: ${h.value}`)
    const resLines =
      rec.outcome.tag === "ok"
        ? rec.outcome.val.headers.map((h) => `< ${h.name}: ${h.value}`)
        : [`< error: ${rec.outcome.val}`]
    detailPre.textContent = [`tab owner: #${rec.owner}`, ...reqLines, "", ...resLines].join("\n")

    details.append(summary, detailPre)
    hopsEl.append(details)
  }
}

function paintCanvas(options: {
  session: SessionState
  targetCanvas: HTMLCanvasElement
  targetOverlay: HTMLCanvasElement
  metaEl: HTMLElement
  width: number
  height: number
}): void {
  const { session, targetCanvas, targetOverlay, metaEl, width, height } = options
  const ctx = targetCanvas.getContext("2d")
  if (!ctx) return
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

    if (paintDiffEnabled && session.lastRgba !== null && targetCanvas === canvasEl) {
      const summary = drawPaintDiffOverlay(targetOverlay, session.lastRgba, frame.rgba)
      if (summary && summary.changedPixels > 0) {
        diffNote = ` · Δ ${summary.changedPixels.toLocaleString()}px (${summary.percent.toFixed(1)}%)`
        if (diffTimer !== null) clearTimeout(diffTimer)
        diffTimer = setTimeout(() => {
          redrawPrimaryOverlay(null)
        }, 650)
      }
    }

    if (targetCanvas === canvasEl) {
      session.lastRgba = new Uint8Array(frame.rgba)
    }

    metaEl.textContent = `${frame.width}×${frame.height} RGBA · ${elapsed}ms${diffNote}`
  } catch (error: unknown) {
    metaEl.textContent = `render failed: ${formatError(error)}`
  }
}

function clearOverlayCanvas(target: HTMLCanvasElement): void {
  const ctx = target.getContext("2d")
  if (ctx) ctx.clearRect(0, 0, target.width, target.height)
}

function redrawPrimaryOverlay(hoverHit: InspectHit | null): void {
  clearOverlayCanvas(overlayEl)
  if (marksEnabled && currentMarks.length > 0) {
    drawAgentMarksOverlay(overlayEl, currentMarks, hoverMarkIndex)
  }
  if (inspectEnabled) {
    const hit = hoverHit ?? pinnedInspect
    if (hit) drawInspectOverlay(overlayEl, hit, hoverHit === null && pinnedInspect !== null)
  }
}

function renderStage(): void {
  if (!sessionA) return
  paintCanvas({
    session: sessionA,
    targetCanvas: canvasEl,
    targetOverlay: overlayEl,
    metaEl: frameMetaEl,
    width: viewportWidth,
    height: viewportHeight,
  })

  if (stageMode === "dual") {
    vpSecondaryEl.hidden = false
    vpPrimaryTitle.textContent = `Desktop ${viewportWidth}×${viewportHeight}`
    vpSecondaryTitle.textContent = `Mobile ${dualMobileWidth}×${viewportHeight}`
    paintCanvas({
      session: sessionA,
      targetCanvas: canvasBEl,
      targetOverlay: overlayBEl,
      metaEl: frameMetaBEl,
      width: dualMobileWidth,
      height: viewportHeight,
    })
    sessionA.tab.setViewport(viewportWidth, viewportHeight)
  } else if (stageMode === "split" && sessionB) {
    vpSecondaryEl.hidden = false
    vpPrimaryTitle.textContent = "Session A (isolated jar)"
    vpSecondaryTitle.textContent = "Session B (isolated jar)"
    paintCanvas({
      session: sessionB,
      targetCanvas: canvasBEl,
      targetOverlay: overlayBEl,
      metaEl: frameMetaBEl,
      width: viewportWidth,
      height: viewportHeight,
    })
  } else {
    vpSecondaryEl.hidden = true
    vpPrimaryTitle.textContent = "Session A"
  }

  if (marksEnabled || activeInspector === "agent") {
    refreshAgentMarks()
  } else {
    redrawPrimaryOverlay(null)
  }
  refreshStorageView()
}

function updateInspector(extra?: string): void {
  const active = getActiveSession()
  if (!active) return
  const url = safeRead(() => active.tab.url())
  const title = safeRead(() => active.tab.title())
  const text = safeRead(() => active.tab.text())
    .replace(/\s+/g, " ")
    .trim()
  const lines = [`[${active.label}] url:   ${url}`, `[${active.label}] title: ${title}`]
  if (extra !== undefined) lines.push(extra)
  lines.push("", text.slice(0, 1200) || "(empty body)")
  outputEl.textContent = lines.join("\n")
  agentTextEl.textContent = text.slice(0, 1600) || "(empty body)"
}

function refreshAgentMarks(): void {
  const active = getActiveSession()
  if (!active) return
  currentMarks = collectAgentMarks((src) => active.tab.eval(src), {
    width: viewportWidth,
    height: viewportHeight,
  })
  agentCountEl.textContent = `${currentMarks.length} interactive ${currentMarks.length === 1 ? "mark" : "marks"}`
  agentMarksListEl.replaceChildren()
  if (currentMarks.length === 0) {
    const empty = document.createElement("p")
    empty.className = "muted"
    empty.textContent = "No visible interactive elements in viewport."
    agentMarksListEl.append(empty)
  } else {
    for (const mark of currentMarks) {
      const btn = document.createElement("button")
      btn.type = "button"
      btn.className = "lab-mark-row"
      btn.textContent = `[${mark.index}] <${mark.tag}> ${mark.label}`
      btn.addEventListener("mouseenter", () => {
        hoverMarkIndex = mark.index
        redrawPrimaryOverlay(null)
      })
      btn.addEventListener("mouseleave", () => {
        hoverMarkIndex = null
        redrawPrimaryOverlay(null)
      })
      btn.addEventListener("click", () => {
        const cx = mark.rect.x + mark.rect.w / 2
        const cy = mark.rect.y + mark.rect.h / 2
        dispatchClickAt(active, { x: cx, y: cy }, `agent clicked [${mark.index}] ${mark.label}`)
      })
      agentMarksListEl.append(btn)
    }
  }
  redrawPrimaryOverlay(null)
}

function refreshStorageView(): void {
  const active = getActiveSession()
  if (!active) return
  storageSessionLabel.textContent = `${active.label} jar`
  const currentUrl = safeRead(() => active.tab.url())
  const cookies = active.context.cookies()
  const localEntries = active.context.localStorage(currentUrl)
  const sessionEntries = active.tab.sessionStorage(currentUrl)

  storageBodyEl.replaceChildren()

  const cookieSection = document.createElement("div")
  cookieSection.className = "lab-storage-section"
  const cookieHead = document.createElement("h3")
  cookieHead.textContent = `Cookies (${cookies.length}) — Rust jar including HttpOnly`
  cookieSection.append(cookieHead)
  if (cookies.length === 0) {
    const empty = document.createElement("p")
    empty.className = "muted"
    empty.textContent = "(empty cookie jar)"
    cookieSection.append(empty)
  } else {
    for (const c of cookies) {
      const row = document.createElement("div")
      row.className = "lab-cookie-row"
      const kv = document.createElement("span")
      kv.textContent = `${c.name}=${c.value}`
      const meta = document.createElement("span")
      meta.className = "muted"
      meta.textContent = `${c.domain}${c.path} · SameSite=${c.sameSite}`
      row.append(kv)
      if (c.httpOnly) {
        const pill = document.createElement("span")
        pill.className = "lab-pill"
        pill.textContent = "HttpOnly"
        row.append(pill)
      }
      if (c.secure) {
        const pill = document.createElement("span")
        pill.className = "lab-pill"
        pill.textContent = "Secure"
        row.append(pill)
      }
      row.append(meta)
      cookieSection.append(row)
    }
  }
  storageBodyEl.append(cookieSection)

  const lsSection = buildStorageSection("localStorage", localEntries)
  const ssSection = buildStorageSection("sessionStorage", sessionEntries)
  storageBodyEl.append(lsSection, ssSection)
}

function buildStorageSection(title: string, entries: Array<WasmStorageEntry>): HTMLElement {
  const section = document.createElement("div")
  section.className = "lab-storage-section"
  const heading = document.createElement("h3")
  heading.textContent = `${title} (${entries.length})`
  section.append(heading)
  if (entries.length === 0) {
    const empty = document.createElement("p")
    empty.className = "muted"
    empty.textContent = "(empty)"
    section.append(empty)
  } else {
    for (const entry of entries) {
      const row = document.createElement("div")
      row.className = "lab-cookie-row"
      row.textContent = `${entry.key} = ${entry.value}`
      section.append(row)
    }
  }
  return section
}

async function pumpSession(session: SessionState, generation: number): Promise<boolean> {
  if (!host) return false
  if (loopPaused) {
    setStatus("paused · click Step 1 tick")
    renderStage()
    return true
  }

  const started = performance.now()
  let steps = 0

  for (;;) {
    if (generation !== pumpGeneration || loopPaused) return false
    const delay = session.tab.step()
    steps += 1

    const events = safeEvents(session.tab)
    const navErr = events.find((item) => item.event.tag === "navigation-failed")
    if (navErr) {
      updateInspector(`navigation-failed: ${String(navErr.event.val ?? "unknown")}`)
      setStatus("navigation failed")
      syncHistoryButtons(session.tab)
      return false
    }

    if (delay === undefined || delay === null) {
      if (host.inFlight() === 0) {
        const elapsed = Math.round(performance.now() - started)
        const currentUrl = safeRead(() => session.tab.url())
        if (
          session === getActiveSession() &&
          (currentUrl.startsWith("http://") || currentUrl.startsWith("https://"))
        ) {
          urlInput.value = currentUrl
        }
        syncHistoryButtons(session.tab)
        updateInspector(`steps: ${steps} · settled in ${elapsed}ms`)
        renderStage()
        setStatus(`idle · ${elapsed}ms`)
        return true
      }
      await new Promise<void>((resolve) => {
        wake = resolve
      })
      continue
    }

    const ms = Number(delay)
    if (ms > 0) {
      await new Promise<void>((resolve) => setTimeout(resolve, Math.min(ms, 250)))
    }
    if (steps > 400) {
      updateInspector(`stopped after ${steps} steps (timer loop)`)
      renderStage()
      setStatus("step cap reached")
      return true
    }
  }
}

function stepOnce(): void {
  const active = getActiveSession()
  if (!active) return
  const started = performance.now()
  const nextDelay = active.tab.step()
  const events = safeEvents(active.tab)
  const elapsed = (performance.now() - started).toFixed(1)
  const eventTags = events.length > 0 ? events.map((e) => e.event.tag).join(", ") : "(no events)"
  const nextText = nextDelay === undefined ? "idle" : `next timer in ${String(nextDelay)}ms`
  updateInspector(`stepped 1 tick (${elapsed}ms) · ${nextText} · events: ${eventTags}`)
  syncHistoryButtons(active.tab)
  renderStage()
  setStatus(`stepped · ${nextText}`)
}

async function navigateSession(session: SessionState, url: string): Promise<boolean> {
  const normalized = /^https?:\/\//i.test(url) ? url : `https://${url}`
  if (session === getActiveSession()) {
    urlInput.value = normalized
  }
  pinnedInspect = null
  session.lastRgba = null
  session.lastSource = { kind: "url", url: normalized }
  pumpGeneration += 1
  const gen = pumpGeneration
  setStatus(`loading ${normalized}…`)
  try {
    session.tab.goto(normalized)
  } catch (error: unknown) {
    updateInspector(`goto failed: ${formatError(error)}`)
    setStatus("error")
    return false
  }
  return pumpSession(session, gen)
}

async function mountSession(session: SessionState, url: string, html: string): Promise<boolean> {
  pinnedInspect = null
  session.lastRgba = null
  session.lastSource = { kind: "html", url, html }
  pumpGeneration += 1
  const gen = pumpGeneration
  setStatus("mounting…")
  try {
    session.tab.mount(url, html)
  } catch (error: unknown) {
    updateInspector(`mount failed: ${formatError(error)}`)
    setStatus("error")
    return false
  }
  return pumpSession(session, gen)
}

async function openUrlInActive(url: string): Promise<boolean> {
  const active = getActiveSession()
  if (!active) return false
  clearHops()
  return navigateSession(active, url)
}

async function openFixtureInActive(url: string, html: string): Promise<boolean> {
  const active = getActiveSession()
  if (!active) return false
  clearHops()
  htmlInput.value = html
  return mountSession(active, url, html)
}

function dispatchInputTo(session: SessionState, event: InputEvent, statusHint?: string): void {
  try {
    session.tab.dispatchInput(event)
  } catch (error: unknown) {
    hintEl.textContent = `input error: ${formatError(error)}`
    return
  }
  if (statusHint !== undefined) {
    hintEl.textContent = statusHint
  }
  if (loopPaused) {
    renderStage()
    return
  }
  pumpGeneration += 1
  void pumpSession(session, pumpGeneration)
}

function dispatchClickAt(
  session: SessionState,
  point: { x: number; y: number },
  label: string,
): void {
  const { x, y } = point
  try {
    session.tab.dispatchInput({ tag: "pointer-down", val: { x, y, button: 0 } })
    session.tab.dispatchInput({ tag: "pointer-up", val: { x, y, button: 0 } })
  } catch (error: unknown) {
    hintEl.textContent = `click error: ${formatError(error)}`
    return
  }
  hintEl.textContent = label
  if (loopPaused) {
    renderStage()
    return
  }
  pumpGeneration += 1
  void pumpSession(session, pumpGeneration)
}

function runEval(sourceOverride?: string, label?: string): void {
  const active = getActiveSession()
  if (!active) return
  const source = (sourceOverride ?? evalInput.value).trim()
  if (!source) return
  if (sourceOverride !== undefined) {
    evalInput.value = sourceOverride
  }
  const started = performance.now()
  try {
    const value = active.tab.eval(source)
    const elapsed = (performance.now() - started).toFixed(1)
    const prefix = label ? `${label} (${elapsed}ms)` : `eval (${elapsed}ms)`
    updateInspector(`${prefix}: ${value}`)
    renderStage()
  } catch (error: unknown) {
    updateInspector(`eval threw: ${formatError(error)}`)
  }
}

function runBoundedInfiniteLoopTest(): void {
  const active = getActiveSession()
  if (!active) return
  const started = performance.now()
  evalInput.value = "while (true) {}"
  try {
    const value = active.tab.evalBounded("while (true) {}", 200n)
    updateInspector(`evalBounded returned: ${value}`)
  } catch (error: unknown) {
    const elapsed = Math.round(performance.now() - started)
    updateInspector(
      `QuickJS interrupted while(true){} after ${elapsed}ms (host browser tab stayed responsive): ${formatError(error)}`,
    )
    hintEl.textContent = `Interrupted infinite loop in ${elapsed}ms — host tab never froze.`
  }
}

function runMultiTabBenchmark(): void {
  if (!engine) return
  const count = 8
  setStatus(`spawning ${count} isolated tabs…`)
  const started = performance.now()
  const results: Array<string> = []

  for (let index = 0; index < count; index += 1) {
    const t0 = performance.now()
    const ctx = new engine.Context()
    const html = `<!doctype html><title>Tab ${index + 1}</title><body><p id="n">${index + 1}</p></body><script>document.getElementById("n").textContent = "tab-" + (${index + 1} * 10);</script>`
    const tab = ctx.openTab(`https://tab-${index + 1}.example/`, html)
    tab.step()
    const value = tab.eval('document.getElementById("n").textContent')
    results.push(
      `tab[${index}] ${tab.title()} -> ${value} (${(performance.now() - t0).toFixed(1)}ms)`,
    )
  }

  const totalMs = (performance.now() - started).toFixed(1)
  outputEl.textContent = [
    `Spawned ${count} independent QuickJS + Blitz contexts in ${totalMs}ms`,
    "",
    ...results,
  ].join("\n")
  setStatus(`${count} contexts spawned · ${totalMs}ms`)
}

function setStageMode(next: StageMode): void {
  stageMode = next
  stageEl.dataset.mode = next
  modeSingleBtn.setAttribute("aria-pressed", String(next === "single"))
  modeDualBtn.setAttribute("aria-pressed", String(next === "dual"))
  modeSplitBtn.setAttribute("aria-pressed", String(next === "split"))

  if (next === "split" && engine && sessionA && sessionB === null) {
    const ctxB = new engine.Context()
    const tabB = ctxB.openTab("http://tinybrowser.invalid/", "")
    sessionB = {
      slot: "b",
      label: "Session B",
      context: ctxB,
      tab: tabB,
      lastRgba: null,
      lastSource: sessionA.lastSource,
    }
    if (sessionA.lastSource.kind === "url") {
      void navigateSession(sessionB, sessionA.lastSource.url)
    } else {
      void mountSession(sessionB, sessionA.lastSource.url, sessionA.lastSource.html)
    }
  }
  if (next !== "split") {
    setActiveSlot("a")
  }
  renderStage()
}

function setInspectorTab(tab: InspectorTab): void {
  activeInspector = tab
  const pairs: Array<[InspectorTab, HTMLButtonElement, HTMLElement]> = [
    ["console", tabConsoleBtn, panelConsole],
    ["network", tabNetworkBtn, panelNetwork],
    ["storage", tabStorageBtn, panelStorage],
    ["agent", tabAgentBtn, panelAgent],
    ["source", tabSourceBtn, panelSource],
  ]
  for (const [key, btn, panel] of pairs) {
    const selected = key === tab
    btn.setAttribute("aria-selected", String(selected))
    panel.hidden = !selected
  }
  if (tab === "storage") refreshStorageView()
  if (tab === "agent") refreshAgentMarks()
}

function canvasPoint(
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

function wireCanvasEvents(canvas: HTMLCanvasElement, slot: SessionSlot): void {
  canvas.addEventListener("pointermove", (event) => {
    if (!inspectEnabled || slot !== "a" || !sessionA) return
    const point = canvasPoint(canvas, event)
    if (!point) return
    const hit = inspectElementAt((src) => sessionA?.tab.eval(src) ?? "", point.x, point.y)
    redrawPrimaryOverlay(hit)
  })

  canvas.addEventListener("pointerleave", () => {
    if (inspectEnabled && slot === "a") {
      redrawPrimaryOverlay(null)
    }
  })

  canvas.addEventListener("pointerdown", (event) => {
    const targetSession = slot === "b" && stageMode === "split" ? sessionB : sessionA
    if (!targetSession) return
    if (stageMode === "split") {
      setActiveSlot(slot)
    }
    const point = canvasPoint(canvas, event)
    if (!point) return
    canvas.focus()

    if (inspectEnabled && slot === "a") {
      pinnedInspect = inspectElementAt((src) => targetSession.tab.eval(src), point.x, point.y)
      redrawPrimaryOverlay(null)
      if (pinnedInspect) {
        updateInspector(
          `inspected <${pinnedInspect.tag}> ${Math.round(pinnedInspect.rect.w)}×${Math.round(pinnedInspect.rect.h)} · font: ${pinnedInspect.font} · color: ${pinnedInspect.color} · display: ${pinnedInspect.display}`,
        )
        hintEl.textContent = `Pinned <${pinnedInspect.tag}> (${pinnedInspect.font}). Toggle Inspect off to click normally.`
      }
      return
    }

    dispatchInputTo(
      targetSession,
      { tag: "pointer-down", val: { x: point.x, y: point.y, button: event.button } },
      `[${targetSession.label}] pointerDown(${Math.round(point.x)}, ${Math.round(point.y)})`,
    )
  })

  canvas.addEventListener("pointerup", (event) => {
    if (inspectEnabled && slot === "a") return
    const targetSession = slot === "b" && stageMode === "split" ? sessionB : sessionA
    if (!targetSession) return
    const point = canvasPoint(canvas, event)
    if (!point) return
    dispatchInputTo(
      targetSession,
      { tag: "pointer-up", val: { x: point.x, y: point.y, button: event.button } },
      `[${targetSession.label}] pointerUp(${Math.round(point.x)}, ${Math.round(point.y)})`,
    )
  })

  canvas.addEventListener("keydown", (event) => {
    const targetSession = slot === "b" && stageMode === "split" ? sessionB : sessionA
    if (!targetSession || event.metaKey || event.ctrlKey) return
    const mapped = WEBDRIVER_KEYS[event.key] ?? (event.key.length === 1 ? event.key : null)
    if (mapped === null) return
    event.preventDefault()
    dispatchInputTo(targetSession, { tag: "key-down", val: mapped }, `keyDown(${event.key})`)
  })

  canvas.addEventListener("keyup", (event) => {
    const targetSession = slot === "b" && stageMode === "split" ? sessionB : sessionA
    if (!targetSession || event.metaKey || event.ctrlKey) return
    const mapped = WEBDRIVER_KEYS[event.key] ?? (event.key.length === 1 ? event.key : null)
    if (mapped === null) return
    event.preventDefault()
    dispatchInputTo(targetSession, { tag: "key-up", val: mapped })
  })

  canvas.addEventListener(
    "wheel",
    (event) => {
      const targetSession = slot === "b" && stageMode === "split" ? sessionB : sessionA
      if (!targetSession) return
      const point = canvasPoint(canvas, event)
      if (!point) return
      event.preventDefault()
      dispatchInputTo(
        targetSession,
        { tag: "wheel", val: { x: point.x, y: point.y, deltaX: 0, deltaY: event.deltaY } },
        `wheel(${Math.round(point.x)}, ${Math.round(point.y)}, dy=${Math.round(event.deltaY)})`,
      )
    },
    { passive: false },
  )
}

async function registerSiteFonts(wasmEngine: WasmEngine): Promise<number> {
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

function enableControls(): void {
  const controls: Array<HTMLButtonElement | HTMLInputElement> = [
    navReloadBtn,
    gotoButton,
    bmHnBtn,
    bmNprBtn,
    bmExampleBtn,
    bmCanvasBtn,
    bmCookiesBtn,
    modeSingleBtn,
    modeDualBtn,
    modeSplitBtn,
    loopPauseBtn,
    loopStepBtn,
    uaTinybrowserBtn,
    uaChromeBtn,
    ovInspectBtn,
    ovMarksBtn,
    ovDiffBtn,
    vpWidthInput,
    scrollUpBtn,
    scrollDownBtn,
    evalButton,
    actionHighlightBtn,
    actionInvertBtn,
    actionLinksBtn,
    actionLoopBtn,
    actionMultitabBtn,
    chaosOfflineBtn,
    chaosLatencyBtn,
    chaosCssBtn,
    chaosJsBtn,
    chaosImgBtn,
    chaosCorsBtn,
    chaos500Btn,
    storageAddBtn,
    storageClearBtn,
    storageRefreshBtn,
    agentScanBtn,
    sourcePullBtn,
    mountButton,
  ]
  for (const control of controls) {
    control.disabled = false
  }
}

async function boot(): Promise<void> {
  setStatus("loading tinybrowser.wasm…")
  const wasmUrl = new URL("/wasm/tinybrowser.js", window.location.origin).href
  const hostUrl = new URL("/wasm/host.js", window.location.origin).href
  try {
    const [wasmMod, hostMod] = (await Promise.all([
      import(/* @vite-ignore */ wasmUrl),
      import(/* @vite-ignore */ hostUrl),
    ])) as [{ engine: WasmEngine }, HostModule]

    engine = wasmMod.engine
    host = hostMod
    host.configure({
      completeFetch: (id, outcome) => wasmMod.engine.completeFetch(id, outcome),
      onStart: recordHopStart,
      onSettled: recordHopSettled,
      onWake: () => {
        if (wake === null) return
        const resume = wake
        wake = null
        resume()
      },
    })

    const families = await registerSiteFonts(engine)
    const ctxA = new engine.Context()
    const tabA = ctxA.openTab("http://tinybrowser.invalid/", "")
    sessionA = {
      slot: "a",
      label: "Session A",
      context: ctxA,
      tab: tabA,
      lastRgba: null,
      lastSource: { kind: "url", url: "https://news.ycombinator.com/" },
    }

    setStatus(`ready · ${families} font ${families === 1 ? "family" : "families"} loaded`)
    enableControls()
    htmlInput.value = interactiveAppHtml

    const loadedLive = await openUrlInActive("https://news.ycombinator.com/")
    if (!loadedLive) {
      await openFixtureInActive("https://demo.tinybrowser.invalid/", interactiveAppHtml)
      hintEl.textContent = "Could not reach /api/hop. Loaded lab://canvas instead."
    }
  } catch (error: unknown) {
    setStatus("wasm bundle not built")
    outputEl.textContent = [
      "Could not load /wasm/tinybrowser.js.",
      "",
      "Build the component locally with:",
      "  cargo build -p tinybrowser-wasm --target wasm32-wasip2 --release",
      "  cd website && bun run build:wasm",
      "",
      `Detail: ${formatError(error)}`,
    ].join("\n")
  }
}

wireCanvasEvents(canvasEl, "a")
wireCanvasEvents(canvasBEl, "b")

gotoButton.addEventListener("click", () => {
  const url = urlInput.value.trim()
  if (url) void openUrlInActive(url)
})

urlInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    const url = urlInput.value.trim()
    if (url) void openUrlInActive(url)
  }
})

navBackBtn.addEventListener("click", () => {
  const active = getActiveSession()
  if (!active) return
  if (active.tab.back()) {
    pumpGeneration += 1
    void pumpSession(active, pumpGeneration)
  }
})

navForwardBtn.addEventListener("click", () => {
  const active = getActiveSession()
  if (!active) return
  if (active.tab.forward()) {
    pumpGeneration += 1
    void pumpSession(active, pumpGeneration)
  }
})

navReloadBtn.addEventListener("click", () => {
  const active = getActiveSession()
  if (!active) return
  if (active.lastSource.kind === "url") {
    void openUrlInActive(active.lastSource.url)
  } else {
    void openFixtureInActive(active.lastSource.url, active.lastSource.html)
  }
})

bmHnBtn.addEventListener("click", () => void openUrlInActive("https://news.ycombinator.com/"))
bmNprBtn.addEventListener("click", () => void openUrlInActive("https://text.npr.org/"))
bmExampleBtn.addEventListener("click", () => void openUrlInActive("https://example.com/"))
bmCanvasBtn.addEventListener(
  "click",
  () => void openFixtureInActive("https://demo.tinybrowser.invalid/", interactiveAppHtml),
)
bmCookiesBtn.addEventListener(
  "click",
  () => void openFixtureInActive(`${window.location.origin}/api/demo/hello.txt`, fetchPresetHtml),
)

modeSingleBtn.addEventListener("click", () => setStageMode("single"))
modeDualBtn.addEventListener("click", () => setStageMode("dual"))
modeSplitBtn.addEventListener("click", () => setStageMode("split"))

loopPauseBtn.addEventListener("click", () => {
  loopPaused = !loopPaused
  loopPauseBtn.setAttribute("aria-pressed", String(loopPaused))
  loopPauseBtn.textContent = loopPaused ? "Paused" : "Live"
  setStatus(loopPaused ? "paused · event loop frozen" : "resumed")
  if (!loopPaused) {
    const active = getActiveSession()
    if (active) {
      pumpGeneration += 1
      void pumpSession(active, pumpGeneration)
    }
  }
})

loopStepBtn.addEventListener("click", stepOnce)

function applyUserAgentPreset(preset: "tinybrowser" | "chrome"): void {
  const active = getActiveSession()
  if (!active) return
  active.tab.setUserAgent(preset)
  uaTinybrowserBtn.setAttribute("aria-pressed", String(preset === "tinybrowser"))
  uaChromeBtn.setAttribute("aria-pressed", String(preset === "chrome"))
  const ua = active.tab.userAgent()
  hintEl.textContent = `User-Agent set to ${ua}`
  if (active.lastSource.kind === "url") {
    void openUrlInActive(active.lastSource.url)
  } else {
    renderStage()
  }
}

uaTinybrowserBtn.addEventListener("click", () => applyUserAgentPreset("tinybrowser"))
uaChromeBtn.addEventListener("click", () => applyUserAgentPreset("chrome"))

ovInspectBtn.addEventListener("click", () => {
  inspectEnabled = !inspectEnabled
  if (!inspectEnabled) pinnedInspect = null
  ovInspectBtn.setAttribute("aria-pressed", String(inspectEnabled))
  redrawPrimaryOverlay(null)
  hintEl.textContent = inspectEnabled
    ? "Inspect mode on: hover the rendered frame to hit-test Blitz layout; click to pin."
    : "Inspect mode off: clicks dispatch to the page."
})

ovMarksBtn.addEventListener("click", () => {
  marksEnabled = !marksEnabled
  ovMarksBtn.setAttribute("aria-pressed", String(marksEnabled))
  if (marksEnabled) {
    refreshAgentMarks()
  } else {
    redrawPrimaryOverlay(null)
  }
})

ovDiffBtn.addEventListener("click", () => {
  paintDiffEnabled = !paintDiffEnabled
  ovDiffBtn.setAttribute("aria-pressed", String(paintDiffEnabled))
  hintEl.textContent = paintDiffEnabled
    ? "Paint diff on: changed pixels flash on every re-render."
    : "Paint diff off."
})

vpWidthInput.addEventListener("input", () => {
  viewportWidth = Number.parseInt(vpWidthInput.value, 10)
  vpWidthLabel.textContent = `${viewportWidth}×${viewportHeight}`
  renderStage()
})

scrollUpBtn.addEventListener("click", () => {
  const active = getActiveSession()
  if (!active) return
  dispatchInputTo(
    active,
    { tag: "wheel", val: { x: viewportWidth / 2, y: viewportHeight / 2, deltaX: 0, deltaY: -240 } },
    "scroll up",
  )
})

scrollDownBtn.addEventListener("click", () => {
  const active = getActiveSession()
  if (!active) return
  dispatchInputTo(
    active,
    { tag: "wheel", val: { x: viewportWidth / 2, y: viewportHeight / 2, deltaX: 0, deltaY: 240 } },
    "scroll down",
  )
})

tabConsoleBtn.addEventListener("click", () => setInspectorTab("console"))
tabNetworkBtn.addEventListener("click", () => setInspectorTab("network"))
tabStorageBtn.addEventListener("click", () => setInspectorTab("storage"))
tabAgentBtn.addEventListener("click", () => setInspectorTab("agent"))
tabSourceBtn.addEventListener("click", () => setInspectorTab("source"))

evalButton.addEventListener("click", () => runEval())
evalInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") runEval()
})

actionHighlightBtn.addEventListener("click", () => {
  runEval(
    `(() => {
      const id = '__tb_hl';
      const existing = document.getElementById(id);
      if (existing) { existing.remove(); return 'highlights removed'; }
      const s = document.createElement('style');
      s.id = id;
      s.textContent = 'h1,h2,h3,a { outline: 2px solid #111 !important; background: #e8e8e8 !important; color: #111 !important; }';
      (document.head || document.documentElement).appendChild(s);
      return 'highlighted ' + document.querySelectorAll('h1,h2,h3,a').length + ' headings and links';
    })()`,
    "highlight",
  )
})

actionInvertBtn.addEventListener("click", () => {
  runEval(
    `(() => {
      const id = '__tb_inv';
      const existing = document.getElementById(id);
      if (existing) { existing.remove(); return 'restored original colors'; }
      const s = document.createElement('style');
      s.id = id;
      s.textContent = 'html, body, table, td, tr, div { background: #111111 !important; color: #ededed !important; border-color: #333333 !important; } a { color: #fcfcfc !important; }';
      (document.head || document.documentElement).appendChild(s);
      return 'injected monochrome dark stylesheet';
    })()`,
    "dark-css",
  )
})

actionLinksBtn.addEventListener("click", () => {
  runEval(
    `Array.from(document.querySelectorAll('a[href]')).slice(0, 12).map((a, i) => (i + 1) + '. ' + (a.textContent || '').trim().slice(0, 50) + ' -> ' + a.href).join('\\n') || '(no links)'`,
    "links",
  )
})

actionLoopBtn.addEventListener("click", runBoundedInfiniteLoopTest)
actionMultitabBtn.addEventListener("click", runMultiTabBenchmark)

function toggleChaosFlag(
  button: HTMLButtonElement,
  key: "offline" | "blockCss" | "blockJs" | "blockImages" | "stripCors" | "force500",
): void {
  if (!host) return
  const current = host.getChaos()[key]
  const next = host.setChaos({ [key]: !current })
  button.setAttribute("aria-pressed", String(next[key]))
}

chaosOfflineBtn.addEventListener("click", () => toggleChaosFlag(chaosOfflineBtn, "offline"))
chaosCssBtn.addEventListener("click", () => toggleChaosFlag(chaosCssBtn, "blockCss"))
chaosJsBtn.addEventListener("click", () => toggleChaosFlag(chaosJsBtn, "blockJs"))
chaosImgBtn.addEventListener("click", () => toggleChaosFlag(chaosImgBtn, "blockImages"))
chaosCorsBtn.addEventListener("click", () => toggleChaosFlag(chaosCorsBtn, "stripCors"))
chaos500Btn.addEventListener("click", () => toggleChaosFlag(chaos500Btn, "force500"))

chaosLatencyBtn.addEventListener("click", () => {
  if (!host) return
  const steps = [0, 400, 1500]
  const current = host.getChaos().latencyMs
  const nextMs = steps[(steps.indexOf(current) + 1) % steps.length] ?? 0
  host.setChaos({ latencyMs: nextMs })
  chaosLatencyBtn.textContent = `Latency: ${nextMs}ms`
  chaosLatencyBtn.setAttribute("aria-pressed", String(nextMs > 0))
})

storageAddBtn.addEventListener("click", () => {
  const active = getActiveSession()
  if (!active) return
  const currentUrl = safeRead(() => active.tab.url())
  const targetUrl =
    currentUrl.startsWith("http://") || currentUrl.startsWith("https://")
      ? currentUrl
      : `${window.location.origin}/`
  const token = Math.random().toString(36).slice(2, 8)
  active.context.setCookie(targetUrl, `lab_secret=${token}; Path=/; HttpOnly`)
  refreshStorageView()
  hintEl.textContent = `Injected HttpOnly cookie lab_secret=${token} into ${active.label}'s Rust jar.`
})

storageClearBtn.addEventListener("click", () => {
  const active = getActiveSession()
  if (!active) return
  const currentUrl = safeRead(() => active.tab.url())
  active.context.clearCookies()
  active.context.clearLocalStorage(currentUrl)
  active.tab.clearSessionStorage(currentUrl)
  refreshStorageView()
})

storageRefreshBtn.addEventListener("click", refreshStorageView)
agentScanBtn.addEventListener("click", refreshAgentMarks)

sourcePullBtn.addEventListener("click", () => {
  const active = getActiveSession()
  if (!active) return
  htmlInput.value = safeRead(() => active.tab.eval("document.documentElement.outerHTML"))
})

mountButton.addEventListener("click", () => {
  const url = `${window.location.origin}/api/demo/hello.txt`
  void openFixtureInActive(url, htmlInput.value)
})

function safeRead(fn: () => string): string {
  try {
    return fn()
  } catch (error: unknown) {
    return `<error: ${formatError(error)}>`
  }
}

function safeEvents(tab: WasmTab): Array<FrameEvent> {
  try {
    return tab.events()
  } catch {
    return []
  }
}

function formatError(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

void boot()
