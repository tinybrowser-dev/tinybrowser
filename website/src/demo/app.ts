/**
 * Interactive driver for the `/demo` page.
 *
 * Loads the jco-transpiled `tinybrowser-wasm` component from `/wasm/`,
 * registers the site's bundled webfonts into the shared font database, and
 * exposes interactive browsing (`goto`, canvas click-to-navigate and button
 * clicks, scroll, viewport resizing, live JS mutation, and streaming HTTP hops).
 */

import fetchPresetHtml from "./fixtures/fetch-cookies.html?raw"
import interactiveAppHtml from "./fixtures/interactive.html?raw"
import type { HopRecord, HopStart } from "./host"

interface RenderedFrame {
  width: number
  height: number
  rgba: Uint8Array
}

interface FrameEvent {
  frame: bigint
  event: { tag: string; val?: unknown }
}

interface WasmTab {
  mount(url: string, html: string): void
  goto(url: string): void
  owner(): bigint
  url(): string
  eval(source: string): string
  render(width: number, height: number): RenderedFrame
  step(): bigint | undefined
  events(): Array<FrameEvent>
  title(): string
  text(): string
}

interface WasmEngine {
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
  inFlight(): number
}

interface HitResult {
  kind: "link" | "click" | "none"
  tag?: string
  href?: string
  label?: string
}

function queryRequired<T extends Element>(selector: string): T {
  const el = document.querySelector<T>(selector)
  if (!el) throw new Error(`missing element: ${selector}`)
  return el
}

const statusEl = queryRequired<HTMLElement>("#demo-status")
const urlInput = queryRequired<HTMLInputElement>("#demo-url")
const gotoButton = queryRequired<HTMLButtonElement>("#demo-goto")
const evalInput = queryRequired<HTMLInputElement>("#demo-eval")
const evalButton = queryRequired<HTMLButtonElement>("#demo-eval-run")
const htmlInput = queryRequired<HTMLTextAreaElement>("#demo-html")
const mountButton = queryRequired<HTMLButtonElement>("#demo-mount")
const renderButton = queryRequired<HTMLButtonElement>("#demo-render")
const canvasEl = queryRequired<HTMLCanvasElement>("#demo-canvas")
const frameMetaEl = queryRequired<HTMLElement>("#demo-frame-meta")
const hintEl = queryRequired<HTMLElement>("#demo-hint")
const outputEl = queryRequired<HTMLElement>("#demo-output")
const hopsEl = queryRequired<HTMLElement>("#demo-hops")
const hopsMetaEl = queryRequired<HTMLElement>("#demo-hops-meta")

const presetHnBtn = queryRequired<HTMLButtonElement>("#preset-hn")
const presetNprBtn = queryRequired<HTMLButtonElement>("#preset-npr")
const presetExampleBtn = queryRequired<HTMLButtonElement>("#preset-example")
const presetInteractiveBtn = queryRequired<HTMLButtonElement>("#preset-interactive")
const presetFetchBtn = queryRequired<HTMLButtonElement>("#preset-fetch")
const presetMultitabBtn = queryRequired<HTMLButtonElement>("#preset-multitab")

const actionHighlightBtn = queryRequired<HTMLButtonElement>("#action-highlight")
const actionLinksBtn = queryRequired<HTMLButtonElement>("#action-links")
const actionInvertBtn = queryRequired<HTMLButtonElement>("#action-invert")
const actionHtmlBtn = queryRequired<HTMLButtonElement>("#action-html")

const vpDesktopBtn = queryRequired<HTMLButtonElement>("#vp-desktop")
const vpTabletBtn = queryRequired<HTMLButtonElement>("#vp-tablet")
const vpMobileBtn = queryRequired<HTMLButtonElement>("#vp-mobile")
const scrollUpBtn = queryRequired<HTMLButtonElement>("#scroll-up")
const scrollDownBtn = queryRequired<HTMLButtonElement>("#scroll-down")

let engine: WasmEngine | null = null
let host: HostModule | null = null
let activeTab: WasmTab | null = null
let wake: (() => void) | null = null
let pumpGeneration = 0
let viewportWidth = 640
let viewportHeight = 400
const hopLines = new Map<number, string>()

function setStatus(message: string): void {
  statusEl.textContent = message
}

function clearHops(): void {
  hopLines.clear()
  hopsEl.textContent = "no requests yet"
  hopsMetaEl.textContent = "via /api/hop"
}

function flushHops(): void {
  if (hopLines.size === 0) {
    hopsEl.textContent = "no requests yet"
    hopsMetaEl.textContent = "via /api/hop"
    return
  }
  hopsEl.textContent = Array.from(hopLines.values()).join("\n")
  hopsMetaEl.textContent = `${hopLines.size} ${hopLines.size === 1 ? "request" : "requests"} via /api/hop`
}

function recordHopStart(req: HopStart): void {
  hopLines.set(req.id, `#${req.id} ${req.method} ${req.url} -> pending…`)
  flushHops()
}

function recordHopSettled(record: HopRecord): void {
  const outcomeText =
    record.outcome.tag === "ok"
      ? `${record.outcome.val.status} (${record.outcome.val.body.byteLength} B)`
      : `ERR ${record.outcome.val}`
  const sentCookie = record.requestHeaders.some((h) => h.name.toLowerCase() === "cookie")
  const gotCookie =
    record.outcome.tag === "ok" &&
    record.outcome.val.headers.some((h) => h.name.toLowerCase() === "set-cookie")
  const location =
    record.outcome.tag === "ok" &&
    record.outcome.val.status >= 300 &&
    record.outcome.val.status < 400
      ? record.outcome.val.headers.find((h) => h.name.toLowerCase() === "location")?.value
      : undefined
  const tags: Array<string> = []
  if (location) tags.push(`-> ${location}`)
  if (sentCookie) tags.push("Cookie")
  if (gotCookie) tags.push("Set-Cookie")
  const suffix = tags.length > 0 ? ` [${tags.join(", ")}]` : ""
  hopLines.set(
    record.id,
    `#${record.id} ${record.method} ${record.url} -> ${outcomeText} · ${record.durationMs}ms${suffix}`,
  )
  flushHops()
}

function renderActiveTab(): void {
  if (!activeTab) return
  const ctx = canvasEl.getContext("2d")
  if (!ctx) return
  const started = performance.now()
  try {
    const frame = activeTab.render(viewportWidth, viewportHeight)
    canvasEl.width = frame.width
    canvasEl.height = frame.height
    const pixels = new Uint8ClampedArray(frame.rgba.byteLength)
    pixels.set(frame.rgba)
    ctx.putImageData(new ImageData(pixels, frame.width, frame.height), 0, 0)
    const elapsed = Math.round(performance.now() - started)
    frameMetaEl.textContent = `${frame.width}×${frame.height} RGBA · painted in ${elapsed}ms`
  } catch (error: unknown) {
    frameMetaEl.textContent = `render failed: ${formatError(error)}`
  }
}

function updateInspector(extra?: string): void {
  if (!activeTab) return
  const url = safeRead(() => activeTab?.url() ?? "")
  const title = safeRead(() => activeTab?.title() ?? "")
  const text = safeRead(() => activeTab?.text() ?? "")
    .replace(/\s+/g, " ")
    .trim()
  const lines = [`url:   ${url}`, `title: ${title}`]
  if (extra !== undefined) lines.push(extra)
  lines.push("", text.slice(0, 1200) || "(empty body)")
  outputEl.textContent = lines.join("\n")
}

async function pumpTab(tab: WasmTab, generation: number): Promise<boolean> {
  if (!host) return false
  const started = performance.now()
  let steps = 0

  for (;;) {
    if (generation !== pumpGeneration) return false
    const delay = tab.step()
    steps += 1

    const events = safeEvents(tab)
    const navErr = events.find((item) => item.event.tag === "navigation-failed")
    if (navErr) {
      updateInspector(`navigation-failed: ${String(navErr.event.val ?? "unknown")}`)
      setStatus("navigation failed")
      return false
    }

    if (delay === undefined || delay === null) {
      if (host.inFlight() === 0) {
        const elapsed = Math.round(performance.now() - started)
        const currentUrl = safeRead(() => tab.url())
        if (currentUrl.startsWith("http://") || currentUrl.startsWith("https://")) {
          urlInput.value = currentUrl
        }
        updateInspector(`steps: ${steps} · settled in ${elapsed}ms`)
        renderActiveTab()
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
      renderActiveTab()
      setStatus("step cap reached")
      return true
    }
  }
}

async function runMount(url: string, html: string): Promise<boolean> {
  if (!engine) return false
  pumpGeneration += 1
  const gen = pumpGeneration
  clearHops()
  setStatus("mounting…")
  try {
    activeTab = new engine.Tab(url, html)
  } catch (error: unknown) {
    outputEl.textContent = `mount failed: ${formatError(error)}`
    setStatus("error")
    return false
  }
  return pumpTab(activeTab, gen)
}

async function runGoto(url: string): Promise<boolean> {
  if (!engine) return false
  const normalized = /^https?:\/\//i.test(url) ? url : `https://${url}`
  urlInput.value = normalized
  pumpGeneration += 1
  const gen = pumpGeneration
  clearHops()
  setStatus(`loading ${normalized}…`)
  try {
    activeTab = new engine.Tab("http://tinybrowser.invalid/", "")
    activeTab.goto(normalized)
  } catch (error: unknown) {
    outputEl.textContent = `goto failed: ${formatError(error)}`
    setStatus("error")
    return false
  }
  return pumpTab(activeTab, gen)
}

function runMultiTabBenchmark(): void {
  if (!engine) return
  pumpGeneration += 1
  clearHops()
  const count = 8
  setStatus(`spawning ${count} tabs…`)
  const started = performance.now()
  const tabs: Array<WasmTab> = []
  const results: Array<string> = []

  for (let index = 0; index < count; index += 1) {
    const t0 = performance.now()
    const html = `<!doctype html><title>Tab ${index + 1}</title><body style="font-family:sans-serif;margin:24px"><h1>Tab ${index + 1}</h1><p id="n">${index + 1}</p></body><script>document.getElementById("n").textContent = "tab-" + (${index + 1} * 10);</script>`
    const tab = new engine.Tab(`https://tab-${index + 1}.example/`, html)
    tab.step()
    const value = tab.eval('document.getElementById("n").textContent')
    tabs.push(tab)
    results.push(
      `tab[${index}] ${tab.title()} -> ${value} (${(performance.now() - t0).toFixed(1)}ms)`,
    )
  }

  const totalMs = (performance.now() - started).toFixed(1)
  activeTab = tabs[tabs.length - 1] ?? null
  outputEl.textContent = [
    `Spawned ${tabs.length} independent QuickJS + Blitz tabs in ${totalMs}ms`,
    "",
    ...results,
  ].join("\n")
  renderActiveTab()
  setStatus(`${tabs.length} live tabs · ${totalMs}ms`)
}

function runEval(sourceOverride?: string, label?: string): void {
  if (!activeTab) return
  const source = (sourceOverride ?? evalInput.value).trim()
  if (!source) return
  if (sourceOverride !== undefined) {
    evalInput.value = sourceOverride
  }
  const started = performance.now()
  try {
    const value = activeTab.eval(source)
    const elapsed = (performance.now() - started).toFixed(1)
    const prefix = label ? `${label} (${elapsed}ms)` : `eval (${elapsed}ms)`
    updateInspector(`${prefix}: ${value}`)
    renderActiveTab()
  } catch (error: unknown) {
    updateInspector(`eval threw: ${formatError(error)}`)
  }
}

async function handleCanvasClick(event: MouseEvent): Promise<void> {
  if (!activeTab) return
  const rect = canvasEl.getBoundingClientRect()
  if (rect.width === 0 || rect.height === 0) return
  const x = Math.round(((event.clientX - rect.left) / rect.width) * canvasEl.width)
  const y = Math.round(((event.clientY - rect.top) / rect.height) * canvasEl.height)

  const hitScript = `(() => {
    const x = ${x}, y = ${y};
    const candidates = Array.from(document.querySelectorAll('a[href], button, input, summary, [onclick]'));
    let best = null;
    let bestArea = Infinity;
    for (const el of candidates) {
      const r = el.getBoundingClientRect();
      if (r.width <= 0 || r.height <= 0) continue;
      if (x >= r.left && x <= r.right && y >= r.top && y <= r.bottom) {
        const area = r.width * r.height;
        if (area < bestArea) {
          best = el;
          bestArea = area;
        }
      }
    }
    if (!best) return JSON.stringify({ kind: 'none' });
    const link = best.closest ? best.closest('a[href]') : (best.tagName === 'A' ? best : null);
    if (link && link.href && (link.href.startsWith('http://') || link.href.startsWith('https://'))) {
      return JSON.stringify({
        kind: 'link',
        tag: 'a',
        href: link.href,
        label: (link.textContent || '').trim().slice(0, 60)
      });
    }
    if (typeof best.click === 'function') best.click();
    return JSON.stringify({
      kind: 'click',
      tag: (best.tagName || '').toLowerCase(),
      label: (best.textContent || best.value || '').trim().slice(0, 60)
    });
  })()`

  let parsed: HitResult
  try {
    parsed = JSON.parse(activeTab.eval(hitScript)) as HitResult
  } catch (error: unknown) {
    hintEl.textContent = `click(${x}, ${y}) error: ${formatError(error)}`
    return
  }

  if (parsed.kind === "link" && parsed.href) {
    hintEl.textContent = `Clicked link (${x}, ${y}) -> ${parsed.href}`
    await runGoto(parsed.href)
    return
  }

  if (parsed.kind === "click") {
    hintEl.textContent = `Clicked <${parsed.tag ?? "el"}> "${parsed.label ?? ""}" at (${x}, ${y})`
    activeTab.step()
    updateInspector(`click(${x}, ${y}) -> <${parsed.tag ?? "el"}> ${parsed.label ?? ""}`)
    renderActiveTab()
    return
  }

  hintEl.textContent = `No link or button at (${x}, ${y}). Try clicking a link or button on the page.`
}

function scrollPage(deltaY: number): void {
  if (!activeTab) return
  try {
    const pos = activeTab.eval(
      `(() => { window.scrollBy(0, ${Math.round(deltaY)}); return Math.round(window.scrollY); })()`,
    )
    renderActiveTab()
    hintEl.textContent = `Scrolled to y=${pos}px`
  } catch (error: unknown) {
    hintEl.textContent = `scroll failed: ${formatError(error)}`
  }
}

function setViewport(width: number, height: number, activeButton: HTMLButtonElement): void {
  viewportWidth = width
  viewportHeight = height
  for (const button of [vpDesktopBtn, vpTabletBtn, vpMobileBtn]) {
    button.setAttribute("aria-pressed", button === activeButton ? "true" : "false")
  }
  renderActiveTab()
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

async function boot(): Promise<void> {
  setStatus("loading tinybrowser.wasm…")
  const wasmUrl = "/wasm/tinybrowser.js"
  const hostUrl = "/wasm/host.js"
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
    setStatus(`ready · ${families} font ${families === 1 ? "family" : "families"} loaded`)

    enableControls()
    htmlInput.value = interactiveAppHtml

    // Load a live site on boot so visitors see live HTTP hops and today's web
    // rendered inside Wasm. Fall back to the interactive app when /api/hop is
    // unavailable: no network, or a Worker that is not running.
    const loadedLive = await runGoto("https://news.ycombinator.com/")
    if (!loadedLive) {
      await runMount("https://demo.tinybrowser.invalid/", interactiveAppHtml)
      hintEl.textContent = "Could not reach /api/hop. Loaded the interactive Wasm app instead."
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

function enableControls(): void {
  const controls = [
    gotoButton,
    evalButton,
    mountButton,
    renderButton,
    presetHnBtn,
    presetNprBtn,
    presetExampleBtn,
    presetInteractiveBtn,
    presetFetchBtn,
    presetMultitabBtn,
    actionHighlightBtn,
    actionLinksBtn,
    actionInvertBtn,
    actionHtmlBtn,
    vpDesktopBtn,
    vpTabletBtn,
    vpMobileBtn,
    scrollUpBtn,
    scrollDownBtn,
  ]
  for (const button of controls) {
    button.disabled = false
  }
}

gotoButton.addEventListener("click", () => {
  const url = urlInput.value.trim()
  if (url) void runGoto(url)
})

urlInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    const url = urlInput.value.trim()
    if (url) void runGoto(url)
  }
})

evalButton.addEventListener("click", () => runEval())
evalInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") runEval()
})

mountButton.addEventListener("click", () => {
  const url = `${window.location.origin}/api/demo/hello.txt`
  void runMount(url, htmlInput.value)
})

renderButton.addEventListener("click", renderActiveTab)

canvasEl.addEventListener("click", (event) => {
  void handleCanvasClick(event)
})

canvasEl.addEventListener(
  "wheel",
  (event) => {
    if (!activeTab) return
    event.preventDefault()
    scrollPage(event.deltaY)
  },
  { passive: false },
)

vpDesktopBtn.addEventListener("click", () => setViewport(800, 500, vpDesktopBtn))
vpTabletBtn.addEventListener("click", () => setViewport(640, 400, vpTabletBtn))
vpMobileBtn.addEventListener("click", () => setViewport(375, 560, vpMobileBtn))
scrollUpBtn.addEventListener("click", () => scrollPage(-240))
scrollDownBtn.addEventListener("click", () => scrollPage(240))

presetHnBtn.addEventListener("click", () => {
  void runGoto("https://news.ycombinator.com/")
})

presetNprBtn.addEventListener("click", () => {
  void runGoto("https://text.npr.org/")
})

presetExampleBtn.addEventListener("click", () => {
  void runGoto("https://example.com/")
})

presetInteractiveBtn.addEventListener("click", () => {
  htmlInput.value = interactiveAppHtml
  void runMount("https://demo.tinybrowser.invalid/", interactiveAppHtml)
})

presetFetchBtn.addEventListener("click", () => {
  htmlInput.value = fetchPresetHtml
  void runMount(`${window.location.origin}/api/demo/hello.txt`, fetchPresetHtml)
})

presetMultitabBtn.addEventListener("click", runMultiTabBenchmark)

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

actionLinksBtn.addEventListener("click", () => {
  runEval(
    `Array.from(document.querySelectorAll('a[href]')).slice(0, 12).map((a, i) => (i + 1) + '. ' + (a.textContent || '').trim().slice(0, 50) + ' -> ' + a.href).join('\\n') || '(no links)'`,
    "links",
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
    "invert",
  )
})

actionHtmlBtn.addEventListener("click", () => {
  if (!activeTab) return
  try {
    const html = activeTab.eval("document.documentElement.outerHTML")
    outputEl.textContent = html.slice(0, 4000)
  } catch (error: unknown) {
    updateInspector(`inspect failed: ${formatError(error)}`)
  }
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
