/**
 * Agent-style driver for the `/demo` page.
 *
 * Loads the jco-transpiled `tinybrowser-wasm` component from `/wasm/`,
 * registers the site's bundled webfonts into the shared font database, and
 * exposes agent-style operations (`goto`, `mount`, `step`, `eval`, `text`,
 * `render`, and multi-tab spawning).
 */

import type { HopRecord } from "./host"

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
    onSettled?: (record: HopRecord) => void
    onWake?: () => void
  }): void
  inFlight(): number
}

const VIEWPORT_WIDTH = 640
const VIEWPORT_HEIGHT = 400

const FETCH_PRESET_HTML = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Fetch + Cookies in Wasm</title>
  <style>
    body { font-family: sans-serif; margin: 24px; color: #111; background: #fcfcfc; }
    h1 { font-size: 20px; margin: 0 0 12px; }
    .card { border: 1px solid #cfcfcf; padding: 14px; background: #f2f2f2; }
    code { font-family: monospace; }
  </style>
</head>
<body>
  <h1>tinybrowser fetch core in Wasm</h1>
  <div class="card">
    <p id="out">Running subresource script, 302 redirect, and cookie echo...</p>
  </div>
  <script src="/api/demo/hello.js"></script>
  <script>
    const out = document.getElementById("out");
    fetch("/api/demo/redirect")
      .then((r) => r.text())
      .then((body) =>
        fetch("/api/demo/echo")
          .then((e) => e.text())
          .then((jar) => {
            out.textContent =
              body.trim() + " | " + jar.trim() + " | " + document.body.dataset.subresource;
          })
      );
  </script>
</body>
</html>`

const LAYOUT_CANVAS_HTML = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Blitz Layout + Canvas 2D</title>
  <style>
    body { font-family: sans-serif; margin: 20px; background: #fcfcfc; color: #111; }
    .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-bottom: 14px; }
    .box { border: 1px solid #111; padding: 12px; background: #f2f2f2; }
    .k { font-size: 12px; color: #575757; margin: 0 0 4px; }
    .v { font-size: 20px; font-weight: 700; margin: 0; }
    canvas { border: 1px solid #111; display: block; }
  </style>
</head>
<body>
  <div class="grid">
    <div class="box"><p class="k">Layout</p><p class="v">Stylo + Taffy</p></div>
    <div class="box"><p class="k">Scripting</p><p class="v" id="js-status">QuickJS</p></div>
  </div>
  <canvas id="c" width="580" height="190"></canvas>
  <script>
    const c = document.getElementById("c");
    const ctx = c.getContext("2d");
    ctx.fillStyle = "#111";
    ctx.fillRect(0, 0, c.width, c.height);
    const bars = [42, 78, 61, 110, 94, 138, 125, 152];
    bars.forEach((h, i) => {
      ctx.fillStyle = i === bars.length - 1 ? "#fcfcfc" : "#777";
      ctx.fillRect(28 + i * 66, c.height - 24 - h, 44, h);
    });
    document.getElementById("js-status").textContent = "QuickJS + Canvas 2D";
  </script>
</body>
</html>`

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
const outputEl = queryRequired<HTMLElement>("#demo-output")
const hopsEl = queryRequired<HTMLElement>("#demo-hops")

const presetFetchBtn = queryRequired<HTMLButtonElement>("#preset-fetch")
const presetExampleBtn = queryRequired<HTMLButtonElement>("#preset-example")
const presetCanvasBtn = queryRequired<HTMLButtonElement>("#preset-canvas")
const presetMultitabBtn = queryRequired<HTMLButtonElement>("#preset-multitab")

let engine: WasmEngine | null = null
let host: HostModule | null = null
let activeTab: WasmTab | null = null
let wake: (() => void) | null = null
let pumpGeneration = 0

function setStatus(message: string): void {
  statusEl.textContent = message
}

function appendHop(record: HopRecord): void {
  const outcomeText =
    record.outcome.tag === "ok"
      ? `${record.outcome.val.status} (${record.outcome.val.body.byteLength} B)`
      : `ERR ${record.outcome.val}`
  const sentCookie = record.requestHeaders.some((h) => h.name.toLowerCase() === "cookie")
  const gotCookie =
    record.outcome.tag === "ok" &&
    record.outcome.val.headers.some((h) => h.name.toLowerCase() === "set-cookie")
  const tags: Array<string> = []
  if (sentCookie) tags.push("Cookie")
  if (gotCookie) tags.push("Set-Cookie")
  const suffix = tags.length > 0 ? ` [${tags.join(", ")}]` : ""
  hopsEl.textContent += `#${record.id} ${record.method} ${record.url} -> ${outcomeText} · ${record.durationMs}ms${suffix}\n`
}

function renderActiveTab(): void {
  if (!activeTab) return
  const ctx = canvasEl.getContext("2d")
  if (!ctx) return
  const started = performance.now()
  try {
    const frame = activeTab.render(VIEWPORT_WIDTH, VIEWPORT_HEIGHT)
    canvasEl.width = frame.width
    canvasEl.height = frame.height
    const pixels = new Uint8ClampedArray(frame.rgba.byteLength)
    pixels.set(frame.rgba)
    ctx.putImageData(new ImageData(pixels, frame.width, frame.height), 0, 0)
    const elapsed = Math.round(performance.now() - started)
    frameMetaEl.textContent = `${frame.width}x${frame.height} RGBA · painted in ${elapsed}ms`
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

async function pumpTab(tab: WasmTab, generation: number): Promise<void> {
  if (!host) return
  const started = performance.now()
  let steps = 0

  for (;;) {
    if (generation !== pumpGeneration) return
    const delay = tab.step()
    steps += 1

    const events = safeEvents(tab)
    const navErr = events.find((item) => item.event.tag === "navigation-failed")
    if (navErr) {
      updateInspector(`navigation-failed: ${String(navErr.event.val ?? "unknown")}`)
      setStatus("navigation failed")
      return
    }

    if (delay === undefined || delay === null) {
      if (host.inFlight() === 0) {
        const elapsed = Math.round(performance.now() - started)
        updateInspector(`steps: ${steps} · settled in ${elapsed}ms`)
        renderActiveTab()
        setStatus(`idle · ${elapsed}ms`)
        return
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
      return
    }
  }
}

async function runMount(url: string, html: string): Promise<void> {
  if (!engine) return
  pumpGeneration += 1
  const gen = pumpGeneration
  hopsEl.textContent = ""
  setStatus("mounting…")
  try {
    activeTab = new engine.Tab(url, html)
  } catch (error: unknown) {
    outputEl.textContent = `mount failed: ${formatError(error)}`
    setStatus("error")
    return
  }
  await pumpTab(activeTab, gen)
}

async function runGoto(url: string): Promise<void> {
  if (!engine) return
  pumpGeneration += 1
  const gen = pumpGeneration
  hopsEl.textContent = ""
  setStatus(`navigating to ${url}…`)
  try {
    activeTab = new engine.Tab("http://tinybrowser.invalid/", "")
    activeTab.goto(url)
  } catch (error: unknown) {
    outputEl.textContent = `goto failed: ${formatError(error)}`
    setStatus("error")
    return
  }
  await pumpTab(activeTab, gen)
}

function runMultiTabBenchmark(): void {
  if (!engine) return
  pumpGeneration += 1
  hopsEl.textContent = ""
  const count = 8
  setStatus(`spawning ${count} tabs…`)
  const started = performance.now()
  const tabs: Array<WasmTab> = []
  const results: Array<string> = []

  for (let index = 0; index < count; index += 1) {
    const t0 = performance.now()
    const html = `<!doctype html><title>Tab ${index + 1}</title><p id="n">${index + 1}</p><script>document.getElementById("n").textContent = "tab-" + (${index + 1} * 10);</script>`
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

function runEval(): void {
  if (!activeTab) return
  const source = evalInput.value.trim()
  if (!source) return
  const started = performance.now()
  try {
    const value = activeTab.eval(source)
    const elapsed = (performance.now() - started).toFixed(1)
    updateInspector(`eval (${elapsed}ms): ${value}`)
    renderActiveTab()
  } catch (error: unknown) {
    updateInspector(`eval threw: ${formatError(error)}`)
  }
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
      onSettled: appendHop,
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
    const originUrl = `${window.location.origin}/api/demo/hello.txt`
    urlInput.value = "https://example.com/"
    htmlInput.value = FETCH_PRESET_HTML
    await runMount(originUrl, FETCH_PRESET_HTML)
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
    presetFetchBtn,
    presetExampleBtn,
    presetCanvasBtn,
    presetMultitabBtn,
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

evalButton.addEventListener("click", runEval)
evalInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") runEval()
})

mountButton.addEventListener("click", () => {
  const url = `${window.location.origin}/api/demo/hello.txt`
  void runMount(url, htmlInput.value)
})

renderButton.addEventListener("click", renderActiveTab)

presetFetchBtn.addEventListener("click", () => {
  htmlInput.value = FETCH_PRESET_HTML
  void runMount(`${window.location.origin}/api/demo/hello.txt`, FETCH_PRESET_HTML)
})

presetExampleBtn.addEventListener("click", () => {
  urlInput.value = "https://example.com/"
  void runGoto("https://example.com/")
})

presetCanvasBtn.addEventListener("click", () => {
  htmlInput.value = LAYOUT_CANVAS_HTML
  void runMount("https://demo.tinybrowser.invalid/", LAYOUT_CANVAS_HTML)
})

presetMultitabBtn.addEventListener("click", runMultiTabBenchmark)

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
