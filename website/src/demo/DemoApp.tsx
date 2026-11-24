import { For, Show, createSignal, onCleanup } from "solid-js"
import fetchPresetHtml from "./fixtures/fetch-cookies.html?raw"
import interactiveAppHtml from "./fixtures/interactive.html?raw"
import type { ChaosConfig, HopRecord, HopStart } from "./host"
import {
  type AgentMark,
  type InspectHit,
  collectAgentMarks,
  drawAgentMarksOverlay,
  drawInspectOverlay,
  inspectElementAt,
} from "./overlays"
import {
  DARK_CSS_SCRIPT,
  DUAL_MOBILE_WIDTH,
  HIGHLIGHT_SCRIPT,
  LINKS_SCRIPT,
  VIEWPORT_HEIGHT,
  WEBDRIVER_KEYS,
  canvasPoint,
  clearOverlayCanvas,
  formatError,
  formatHopDetail,
  formatHopSummary,
  paintCanvas,
  registerSiteFonts,
  safeEvents,
  safeRead,
  type HopEntry,
  type HostModule,
  type InputEvent,
  type InspectorTab,
  type SessionSlot,
  type SessionState,
  type StageMode,
  type UserAgentPreset,
  type WasmCookie,
  type WasmEngine,
  type WasmStorageEntry,
  type WasmTab,
} from "./engine"

const DEFAULT_CHAOS: ChaosConfig = {
  offline: false,
  latencyMs: 0,
  blockCss: false,
  blockJs: false,
  blockImages: false,
  stripCors: false,
  force500: false,
}

const ariaBool = (value: boolean): "true" | "false" => (value ? "true" : "false")

export function DemoApp() {
  const [ready, setReady] = createSignal(false)
  const [status, setStatus] = createSignal("initializing…")
  const [hint, setHint] = createSignal(
    "tinybrowser.wasm paints this canvas in your tab. Click, scroll, or type inside.",
  )
  const [url, setUrl] = createSignal("")
  const [canGoBack, setCanGoBack] = createSignal(false)
  const [canGoForward, setCanGoForward] = createSignal(false)

  const [devtoolsOpen, setDevtoolsOpen] = createSignal(false)
  const [activeTab, setActiveTab] = createSignal<InspectorTab>("console")
  const [stageMode, setStageMode] = createSignal<StageMode>("single")
  const [activeSlot, setActiveSlotSignal] = createSignal<SessionSlot>("a")
  const [viewportWidth, setViewportWidth] = createSignal(640)

  const [loopPaused, setLoopPaused] = createSignal(false)
  const [uaPreset, setUaPreset] = createSignal<UserAgentPreset>("tinybrowser")
  const [inspectEnabled, setInspectEnabled] = createSignal(false)
  const [marksEnabled, setMarksEnabled] = createSignal(false)
  const [paintDiffEnabled, setPaintDiffEnabled] = createSignal(false)

  const [primaryTitle, setPrimaryTitle] = createSignal("Session A")
  const [primaryMeta, setPrimaryMeta] = createSignal("640×400 RGBA")
  const [secondaryTitle, setSecondaryTitle] = createSignal("Mobile 375×400")
  const [secondaryMeta, setSecondaryMeta] = createSignal("375×400 RGBA")

  const [evalSource, setEvalSource] = createSignal(
    "document.title + ' · ' + document.querySelectorAll('*').length + ' nodes'",
  )
  const [consoleOutput, setConsoleOutput] = createSignal("waiting for engine…")
  const [agentText, setAgentText] = createSignal("")
  const [sourceHtml, setSourceHtml] = createSignal(interactiveAppHtml)

  const [hops, setHops] = createSignal<Array<HopEntry>>([])
  const [chaos, setChaos] = createSignal<ChaosConfig>({ ...DEFAULT_CHAOS })

  const [storageLabel, setStorageLabel] = createSignal("Session A jar")
  const [cookies, setCookies] = createSignal<Array<WasmCookie>>([])
  const [localEntries, setLocalEntries] = createSignal<Array<WasmStorageEntry>>([])
  const [sessionEntries, setSessionEntries] = createSignal<Array<WasmStorageEntry>>([])
  const [marks, setMarks] = createSignal<Array<AgentMark>>([])

  let canvasEl: HTMLCanvasElement | undefined
  let overlayEl: HTMLCanvasElement | undefined
  let canvasBEl: HTMLCanvasElement | undefined
  let overlayBEl: HTMLCanvasElement | undefined

  let engine: WasmEngine | null = null
  let host: HostModule | null = null
  let sessionA: SessionState | null = null
  let sessionB: SessionState | null = null
  let pinnedInspect: InspectHit | null = null
  let hoverMarkIndex: number | null = null
  let wake: (() => void) | null = null
  let pumpGeneration = 0
  let diffTimer: ReturnType<typeof setTimeout> | null = null
  const hopMap = new Map<number, HopEntry>()

  function getActiveSession(): SessionState | null {
    return activeSlot() === "b" && sessionB !== null ? sessionB : sessionA
  }

  function syncHistoryButtons(tab: WasmTab): void {
    setCanGoBack(tab.canGoBack())
    setCanGoForward(tab.canGoForward())
  }

  function clearHops(): void {
    hopMap.clear()
    setHops([])
  }

  function recordHopStart(req: HopStart): void {
    hopMap.set(req.id, { id: req.id, owner: req.owner, method: req.method, url: req.url })
    setHops(Array.from(hopMap.values()).reverse())
  }

  function recordHopSettled(record: HopRecord): void {
    hopMap.set(record.id, {
      id: record.id,
      owner: record.owner,
      method: record.method,
      url: record.url,
      record,
    })
    setHops(Array.from(hopMap.values()).reverse())
  }

  function redrawPrimaryOverlay(hoverHit: InspectHit | null): void {
    if (!overlayEl) return
    clearOverlayCanvas(overlayEl)
    const currentMarks = marks()
    if (marksEnabled() && currentMarks.length > 0) {
      drawAgentMarksOverlay(overlayEl, currentMarks, hoverMarkIndex)
    }
    if (inspectEnabled()) {
      const hit = hoverHit ?? pinnedInspect
      if (hit) drawInspectOverlay(overlayEl, hit, hoverHit === null && pinnedInspect !== null)
    }
  }

  function refreshAgentMarks(): void {
    const active = getActiveSession()
    if (!active) return
    const collected = collectAgentMarks((src) => active.tab.eval(src), {
      width: viewportWidth(),
      height: VIEWPORT_HEIGHT,
    })
    setMarks(collected)
    redrawPrimaryOverlay(null)
  }

  function refreshStorageView(): void {
    const active = getActiveSession()
    if (!active) return
    setStorageLabel(`${active.label} jar`)
    const currentUrl = safeRead(() => active.tab.url())
    setCookies(active.context.cookies())
    setLocalEntries(active.context.localStorage(currentUrl))
    setSessionEntries(active.tab.sessionStorage(currentUrl))
  }

  function renderStage(): void {
    if (!sessionA || !canvasEl || !overlayEl) return
    const width = viewportWidth()
    const mode = stageMode()

    if (diffTimer !== null) {
      clearTimeout(diffTimer)
      diffTimer = null
    }

    const primaryResult = paintCanvas({
      session: sessionA,
      targetCanvas: canvasEl,
      targetOverlay: overlayEl,
      width,
      height: VIEWPORT_HEIGHT,
      isPrimary: true,
      paintDiffEnabled: paintDiffEnabled(),
      onClearDiff: () => redrawPrimaryOverlay(null),
    })
    setPrimaryMeta(primaryResult.meta)
    if (primaryResult.diffTimer) diffTimer = primaryResult.diffTimer

    if (mode === "dual" && canvasBEl && overlayBEl) {
      setPrimaryTitle(`Desktop ${width}×${VIEWPORT_HEIGHT}`)
      setSecondaryTitle(`Mobile ${DUAL_MOBILE_WIDTH}×${VIEWPORT_HEIGHT}`)
      const sec = paintCanvas({
        session: sessionA,
        targetCanvas: canvasBEl,
        targetOverlay: overlayBEl,
        width: DUAL_MOBILE_WIDTH,
        height: VIEWPORT_HEIGHT,
        isPrimary: false,
        paintDiffEnabled: false,
        onClearDiff: () => {},
      })
      setSecondaryMeta(sec.meta)
      sessionA.tab.setViewport(width, VIEWPORT_HEIGHT)
    } else if (mode === "split" && sessionB && canvasBEl && overlayBEl) {
      setPrimaryTitle("Session A (isolated jar)")
      setSecondaryTitle("Session B (isolated jar)")
      const sec = paintCanvas({
        session: sessionB,
        targetCanvas: canvasBEl,
        targetOverlay: overlayBEl,
        width,
        height: VIEWPORT_HEIGHT,
        isPrimary: false,
        paintDiffEnabled: false,
        onClearDiff: () => {},
      })
      setSecondaryMeta(sec.meta)
    } else {
      setPrimaryTitle("Session A")
    }

    if (marksEnabled() || activeTab() === "agent") {
      refreshAgentMarks()
    } else {
      redrawPrimaryOverlay(null)
    }
    refreshStorageView()
  }

  function updateInspector(extra?: string): void {
    const active = getActiveSession()
    if (!active) return
    const tabUrl = safeRead(() => active.tab.url())
    const title = safeRead(() => active.tab.title())
    const text = safeRead(() => active.tab.text())
      .replace(/\s+/g, " ")
      .trim()
    const lines = [`[${active.label}] url:   ${tabUrl}`, `[${active.label}] title: ${title}`]
    if (extra !== undefined) lines.push(extra)
    lines.push("", text.slice(0, 1200) || "(empty body)")
    setConsoleOutput(lines.join("\n"))
    setAgentText(text.slice(0, 1600) || "(empty body)")
  }

  function setActiveSlot(slot: SessionSlot): void {
    setActiveSlotSignal(slot)
    const active = getActiveSession()
    if (!active) return
    const currentUrl = safeRead(() => active.tab.url())
    if (currentUrl.startsWith("http://") || currentUrl.startsWith("https://")) {
      setUrl(currentUrl)
    }
    syncHistoryButtons(active.tab)
    refreshStorageView()
    if (marksEnabled() || activeTab() === "agent") {
      refreshAgentMarks()
    }
  }

  async function pumpSession(session: SessionState, generation: number): Promise<boolean> {
    if (!host) return false
    if (loopPaused()) {
      setStatus("paused · click Step 1 tick")
      renderStage()
      return true
    }

    const started = performance.now()
    let steps = 0

    for (;;) {
      if (generation !== pumpGeneration || loopPaused()) return false
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
            setUrl(currentUrl)
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

  async function navigateSession(session: SessionState, targetUrl: string): Promise<boolean> {
    const normalized = /^https?:\/\//i.test(targetUrl) ? targetUrl : `https://${targetUrl}`
    if (session === getActiveSession()) {
      setUrl(normalized)
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

  async function mountSession(
    session: SessionState,
    targetUrl: string,
    html: string,
  ): Promise<boolean> {
    pinnedInspect = null
    session.lastRgba = null
    session.lastSource = { kind: "html", url: targetUrl, html }
    pumpGeneration += 1
    const gen = pumpGeneration
    setStatus("mounting…")
    try {
      session.tab.mount(targetUrl, html)
    } catch (error: unknown) {
      updateInspector(`mount failed: ${formatError(error)}`)
      setStatus("error")
      return false
    }
    return pumpSession(session, gen)
  }

  async function openUrlInActive(targetUrl: string): Promise<boolean> {
    const active = getActiveSession()
    if (!active) return false
    clearHops()
    return navigateSession(active, targetUrl)
  }

  async function openFixtureInActive(targetUrl: string, html: string): Promise<boolean> {
    const active = getActiveSession()
    if (!active) return false
    clearHops()
    setSourceHtml(html)
    return mountSession(active, targetUrl, html)
  }

  function dispatchInputTo(session: SessionState, event: InputEvent, statusHint?: string): void {
    try {
      session.tab.dispatchInput(event)
    } catch (error: unknown) {
      setHint(`input error: ${formatError(error)}`)
      return
    }
    if (statusHint !== undefined) setHint(statusHint)
    if (loopPaused()) {
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
    try {
      session.tab.dispatchInput({ tag: "pointer-down", val: { x: point.x, y: point.y, button: 0 } })
      session.tab.dispatchInput({ tag: "pointer-up", val: { x: point.x, y: point.y, button: 0 } })
    } catch (error: unknown) {
      setHint(`click error: ${formatError(error)}`)
      return
    }
    setHint(label)
    if (loopPaused()) {
      renderStage()
      return
    }
    pumpGeneration += 1
    void pumpSession(session, pumpGeneration)
  }

  function runEval(sourceOverride?: string, label?: string): void {
    const active = getActiveSession()
    if (!active) return
    const source = (sourceOverride ?? evalSource()).trim()
    if (!source) return
    if (sourceOverride !== undefined) setEvalSource(sourceOverride)
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
    setEvalSource("while (true) {}")
    try {
      const value = active.tab.evalBounded("while (true) {}", 200n)
      updateInspector(`evalBounded returned: ${value}`)
    } catch (error: unknown) {
      const elapsed = Math.round(performance.now() - started)
      updateInspector(
        `QuickJS interrupted while(true){} after ${elapsed}ms (host browser tab stayed responsive): ${formatError(error)}`,
      )
      setHint(`Interrupted infinite loop in ${elapsed}ms — host tab never froze.`)
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
    setConsoleOutput(
      [
        `Spawned ${count} independent QuickJS + Blitz contexts in ${totalMs}ms`,
        "",
        ...results,
      ].join("\n"),
    )
    setStatus(`${count} contexts spawned · ${totalMs}ms`)
  }

  function changeStageMode(next: StageMode): void {
    setStageMode(next)
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
    if (next !== "split") setActiveSlot("a")
    queueMicrotask(() => renderStage())
  }

  function selectInspectorTab(tab: InspectorTab): void {
    setActiveTab(tab)
    if (tab === "storage") refreshStorageView()
    if (tab === "agent") refreshAgentMarks()
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

  function toggleLoopPaused(): void {
    const next = !loopPaused()
    setLoopPaused(next)
    setStatus(next ? "paused · event loop frozen" : "resumed")
    if (!next) {
      const active = getActiveSession()
      if (active) {
        pumpGeneration += 1
        void pumpSession(active, pumpGeneration)
      }
    }
  }

  function applyUserAgent(preset: UserAgentPreset): void {
    const active = getActiveSession()
    if (!active) return
    active.tab.setUserAgent(preset)
    setUaPreset(preset)
    setHint(`User-Agent set to ${active.tab.userAgent()}`)
    if (active.lastSource.kind === "url") {
      void openUrlInActive(active.lastSource.url)
    } else {
      renderStage()
    }
  }

  function toggleChaos(
    key: "offline" | "blockCss" | "blockJs" | "blockImages" | "stripCors" | "force500",
  ): void {
    if (!host) return
    const next = host.setChaos({ [key]: !host.getChaos()[key] })
    setChaos({ ...next })
  }

  function cycleLatency(): void {
    if (!host) return
    const steps = [0, 400, 1500]
    const current = host.getChaos().latencyMs
    const nextMs = steps[(steps.indexOf(current) + 1) % steps.length] ?? 0
    const next = host.setChaos({ latencyMs: nextMs })
    setChaos({ ...next })
  }

  function canvasHandlers(slot: SessionSlot) {
    return {
      onPointerMove: (event: PointerEvent & { currentTarget: HTMLCanvasElement }) => {
        if (!inspectEnabled() || slot !== "a" || !sessionA) return
        const point = canvasPoint(event.currentTarget, event)
        if (!point) return
        const hit = inspectElementAt((src) => sessionA?.tab.eval(src) ?? "", point.x, point.y)
        redrawPrimaryOverlay(hit)
      },
      onPointerLeave: () => {
        if (inspectEnabled() && slot === "a") redrawPrimaryOverlay(null)
      },
      onPointerDown: (event: PointerEvent & { currentTarget: HTMLCanvasElement }) => {
        const targetSession = slot === "b" && stageMode() === "split" ? sessionB : sessionA
        if (!targetSession) return
        if (stageMode() === "split") setActiveSlot(slot)
        const point = canvasPoint(event.currentTarget, event)
        if (!point) return
        event.currentTarget.focus()

        if (inspectEnabled() && slot === "a") {
          pinnedInspect = inspectElementAt((src) => targetSession.tab.eval(src), point.x, point.y)
          redrawPrimaryOverlay(null)
          if (pinnedInspect) {
            updateInspector(
              `inspected <${pinnedInspect.tag}> ${Math.round(pinnedInspect.rect.w)}×${Math.round(pinnedInspect.rect.h)} · font: ${pinnedInspect.font} · color: ${pinnedInspect.color} · display: ${pinnedInspect.display}`,
            )
            setHint(
              `Pinned <${pinnedInspect.tag}> (${pinnedInspect.font}). Toggle Element inspector off to click normally.`,
            )
          }
          return
        }

        dispatchInputTo(
          targetSession,
          { tag: "pointer-down", val: { x: point.x, y: point.y, button: event.button } },
          `[${targetSession.label}] pointerDown(${Math.round(point.x)}, ${Math.round(point.y)})`,
        )
      },
      onPointerUp: (event: PointerEvent & { currentTarget: HTMLCanvasElement }) => {
        if (inspectEnabled() && slot === "a") return
        const targetSession = slot === "b" && stageMode() === "split" ? sessionB : sessionA
        if (!targetSession) return
        const point = canvasPoint(event.currentTarget, event)
        if (!point) return
        dispatchInputTo(
          targetSession,
          { tag: "pointer-up", val: { x: point.x, y: point.y, button: event.button } },
          `[${targetSession.label}] pointerUp(${Math.round(point.x)}, ${Math.round(point.y)})`,
        )
      },
      onKeyDown: (event: KeyboardEvent) => {
        const targetSession = slot === "b" && stageMode() === "split" ? sessionB : sessionA
        if (!targetSession || event.metaKey || event.ctrlKey) return
        const mapped = WEBDRIVER_KEYS[event.key] ?? (event.key.length === 1 ? event.key : null)
        if (mapped === null) return
        event.preventDefault()
        dispatchInputTo(targetSession, { tag: "key-down", val: mapped }, `keyDown(${event.key})`)
      },
      onKeyUp: (event: KeyboardEvent) => {
        const targetSession = slot === "b" && stageMode() === "split" ? sessionB : sessionA
        if (!targetSession || event.metaKey || event.ctrlKey) return
        const mapped = WEBDRIVER_KEYS[event.key] ?? (event.key.length === 1 ? event.key : null)
        if (mapped === null) return
        event.preventDefault()
        dispatchInputTo(targetSession, { tag: "key-up", val: mapped })
      },
      onWheel: (event: WheelEvent & { currentTarget: HTMLCanvasElement }) => {
        const targetSession = slot === "b" && stageMode() === "split" ? sessionB : sessionA
        if (!targetSession) return
        const point = canvasPoint(event.currentTarget, event)
        if (!point) return
        event.preventDefault()
        dispatchInputTo(
          targetSession,
          { tag: "wheel", val: { x: point.x, y: point.y, deltaX: 0, deltaY: event.deltaY } },
          `wheel(${Math.round(point.x)}, ${Math.round(point.y)}, dy=${Math.round(event.deltaY)})`,
        )
      },
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
      setReady(true)

      const loadedLive = await openUrlInActive("https://news.ycombinator.com/")
      if (!loadedLive) {
        await openFixtureInActive("https://demo.tinybrowser.invalid/", interactiveAppHtml)
        setHint("Could not reach /api/hop. Loaded lab://canvas instead.")
      }
    } catch (error: unknown) {
      setStatus("wasm bundle not built")
      setConsoleOutput(
        [
          "Could not load /wasm/tinybrowser.js.",
          "",
          "Build the component locally with:",
          "  cargo build -p tinybrowser-wasm --target wasm32-wasip2 --release",
          "  cd website && bun run build:wasm",
          "",
          `Detail: ${formatError(error)}`,
        ].join("\n"),
      )
    }
  }

  onCleanup(() => {
    if (diffTimer !== null) clearTimeout(diffTimer)
  })
  queueMicrotask(() => {
    void boot()
  })

  const primaryHandlers = canvasHandlers("a")
  const secondaryHandlers = canvasHandlers("b")

  return (
    <>
      <div class="page-head">
        <h1 class="page-title">Demo</h1>
        <p class="run-meta">{status()}</p>
      </div>

      <section class="demo-window" aria-label="Browser viewport">
        <div class="demo-omnibox">
          <div class="demo-nav-btns" role="group" aria-label="Session history">
            <button
              type="button"
              disabled={!canGoBack()}
              title="Back"
              aria-label="Back"
              onClick={() => {
                const active = getActiveSession()
                if (active && active.tab.back()) {
                  pumpGeneration += 1
                  void pumpSession(active, pumpGeneration)
                }
              }}
            >
              <svg
                class="demo-icon"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="1.1"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
              >
                <path d="M19 12H5m6-6-6 6 6 6" />
              </svg>
            </button>
            <button
              type="button"
              disabled={!canGoForward()}
              title="Forward"
              aria-label="Forward"
              onClick={() => {
                const active = getActiveSession()
                if (active && active.tab.forward()) {
                  pumpGeneration += 1
                  void pumpSession(active, pumpGeneration)
                }
              }}
            >
              <svg
                class="demo-icon"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="1.1"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
              >
                <path d="M5 12h14m-6-6 6 6-6 6" />
              </svg>
            </button>
            <button
              type="button"
              disabled={!ready()}
              title="Reload"
              aria-label="Reload"
              onClick={() => {
                const active = getActiveSession()
                if (!active) return
                if (active.lastSource.kind === "url") {
                  void openUrlInActive(active.lastSource.url)
                } else {
                  void openFixtureInActive(active.lastSource.url, active.lastSource.html)
                }
              }}
            >
              <svg
                class="demo-icon"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="1.1"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
              >
                <path d="M20 12a8 8 0 1 1-2.34-5.66L20 8.5M20 4v4.5h-4.5" />
              </svg>
            </button>
          </div>
          <input
            type="url"
            placeholder="https://news.ycombinator.com/"
            spellcheck={false}
            aria-label="URL"
            value={url()}
            onInput={(e) => setUrl(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                const target = url().trim()
                if (target) void openUrlInActive(target)
              }
            }}
          />
          <button
            class="demo-action"
            type="button"
            disabled={!ready()}
            onClick={() => {
              const target = url().trim()
              if (target) void openUrlInActive(target)
            }}
          >
            Go
          </button>
          <button
            class="demo-action"
            type="button"
            aria-expanded={ariaBool(devtoolsOpen())}
            aria-pressed={ariaBool(devtoolsOpen())}
            aria-controls="demo-devtools"
            onClick={() => setDevtoolsOpen((open) => !open)}
          >
            DevTools
          </button>
        </div>

        <div class="demo-bookmarks" role="group" aria-label="Quick URLs">
          <button
            type="button"
            disabled={!ready()}
            onClick={() => void openUrlInActive("https://news.ycombinator.com/")}
          >
            HN
          </button>
          <button
            type="button"
            disabled={!ready()}
            onClick={() => void openUrlInActive("https://text.npr.org/")}
          >
            NPR
          </button>
          <button
            type="button"
            disabled={!ready()}
            onClick={() => void openUrlInActive("https://example.com/")}
          >
            example.com
          </button>
          <button
            type="button"
            disabled={!ready()}
            onClick={() =>
              void openFixtureInActive("https://demo.tinybrowser.invalid/", interactiveAppHtml)
            }
          >
            lab://canvas
          </button>
          <button
            type="button"
            disabled={!ready()}
            onClick={() =>
              void openFixtureInActive(
                `${window.location.origin}/api/demo/hello.txt`,
                fetchPresetHtml,
              )
            }
          >
            lab://cookies
          </button>
        </div>

        <div class="lab-stage" data-mode={stageMode()}>
          <div class="lab-viewport" data-active={String(activeSlot() === "a")}>
            <div class="lab-vp-head">
              <span class="strong">{primaryTitle()}</span>
              <span class="muted">{primaryMeta()}</span>
            </div>
            <div class="lab-canvas-stack">
              <canvas
                ref={(el) => {
                  canvasEl = el
                }}
                width={640}
                height={VIEWPORT_HEIGHT}
                tabindex={0}
                title="Click, scroll, or type inside the rendered page."
                {...primaryHandlers}
              />
              <canvas
                ref={(el) => {
                  overlayEl = el
                }}
                class="lab-overlay-canvas"
                width={640}
                height={VIEWPORT_HEIGHT}
                aria-hidden="true"
              />
            </div>
          </div>

          <div
            class="lab-viewport"
            hidden={stageMode() === "single"}
            data-active={String(activeSlot() === "b")}
          >
            <div class="lab-vp-head">
              <span class="strong">{secondaryTitle()}</span>
              <span class="muted">{secondaryMeta()}</span>
            </div>
            <div class="lab-canvas-stack">
              <canvas
                ref={(el) => {
                  canvasBEl = el
                }}
                width={DUAL_MOBILE_WIDTH}
                height={VIEWPORT_HEIGHT}
                tabindex={0}
                title="Secondary viewport / Session B"
                {...secondaryHandlers}
              />
              <canvas
                ref={(el) => {
                  overlayBEl = el
                }}
                class="lab-overlay-canvas"
                width={DUAL_MOBILE_WIDTH}
                height={VIEWPORT_HEIGHT}
                aria-hidden="true"
              />
            </div>
          </div>
        </div>
      </section>

      <p class="demo-hint">{hint()}</p>

      <aside
        class="lab-inspector"
        id="demo-devtools"
        hidden={!devtoolsOpen()}
        aria-label="Engine inspector"
      >
        <div class="lab-tabs" role="tablist">
          {(
            [
              ["console", "Console"],
              ["network", "Network"],
              ["storage", "Storage"],
              ["agent", "Agent"],
              ["source", "Source"],
              ["lab", "Lab"],
            ] as const
          ).map(([id, label]) => (
            <button
              role="tab"
              type="button"
              aria-selected={ariaBool(activeTab() === id)}
              onClick={() => selectInspectorTab(id)}
            >
              {label}
              <Show when={id === "network"}>
                {" "}
                <span class="muted">{hops().length}</span>
              </Show>
            </button>
          ))}
        </div>

        <div class="lab-panel" role="tabpanel" hidden={activeTab() !== "console"}>
          <div class="demo-row">
            <input
              type="text"
              spellcheck={false}
              aria-label="JavaScript expression"
              value={evalSource()}
              onInput={(e) => setEvalSource(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") runEval()
              }}
            />
            <button class="demo-action" type="button" disabled={!ready()} onClick={() => runEval()}>
              Run
            </button>
          </div>
          <div class="lab-chips">
            <button
              type="button"
              disabled={!ready()}
              onClick={() => runEval(HIGHLIGHT_SCRIPT, "highlight")}
            >
              Highlight
            </button>
            <button
              type="button"
              disabled={!ready()}
              onClick={() => runEval(DARK_CSS_SCRIPT, "dark-css")}
            >
              Dark CSS
            </button>
            <button
              type="button"
              disabled={!ready()}
              onClick={() => runEval(LINKS_SCRIPT, "links")}
            >
              Links
            </button>
            <button type="button" disabled={!ready()} onClick={runBoundedInfiniteLoopTest}>
              {"while(true){}"}
            </button>
            <button type="button" disabled={!ready()} onClick={runMultiTabBenchmark}>
              Spawn 8 tabs
            </button>
          </div>
          <pre class="demo-pre lab-pre-tall">{consoleOutput()}</pre>
        </div>

        <div class="lab-panel" role="tabpanel" hidden={activeTab() !== "network"}>
          <p class="lab-note">
            {hops().length} {hops().length === 1 ? "request" : "requests"} via /api/hop
          </p>
          <div class="lab-hops-list">
            <For each={hops()} fallback={<p class="muted">No requests yet.</p>}>
              {(entry) => (
                <details class="lab-hop-item">
                  <summary>{formatHopSummary(entry)}</summary>
                  <Show when={entry.record}>
                    {(rec) => <pre class="lab-hop-detail">{formatHopDetail(rec())}</pre>}
                  </Show>
                </details>
              )}
            </For>
          </div>
          <details class="lab-chaos-details">
            <summary>Network chaos</summary>
            <div class="lab-chaos" aria-label="Network chaos controls">
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(chaos().offline)}
                onClick={() => toggleChaos("offline")}
              >
                Offline
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(chaos().latencyMs > 0)}
                onClick={cycleLatency}
              >
                Latency: {chaos().latencyMs}ms
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(chaos().blockCss)}
                onClick={() => toggleChaos("blockCss")}
              >
                Block CSS
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(chaos().blockJs)}
                onClick={() => toggleChaos("blockJs")}
              >
                Block JS
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(chaos().blockImages)}
                onClick={() => toggleChaos("blockImages")}
              >
                Block Img
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(chaos().stripCors)}
                onClick={() => toggleChaos("stripCors")}
              >
                Strip CORS
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(chaos().force500)}
                onClick={() => toggleChaos("force500")}
              >
                Force 500
              </button>
            </div>
          </details>
        </div>

        <div class="lab-panel" role="tabpanel" hidden={activeTab() !== "storage"}>
          <div class="lab-storage-bar">
            <span class="strong">{storageLabel()}</span>
            <button
              type="button"
              disabled={!ready()}
              onClick={() => {
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
                setHint(
                  `Injected HttpOnly cookie lab_secret=${token} into ${active.label}'s Rust jar.`,
                )
              }}
            >
              + HttpOnly cookie
            </button>
            <button
              type="button"
              disabled={!ready()}
              onClick={() => {
                const active = getActiveSession()
                if (!active) return
                const currentUrl = safeRead(() => active.tab.url())
                active.context.clearCookies()
                active.context.clearLocalStorage(currentUrl)
                active.tab.clearSessionStorage(currentUrl)
                refreshStorageView()
              }}
            >
              Clear all
            </button>
            <button type="button" disabled={!ready()} onClick={refreshStorageView}>
              Refresh
            </button>
          </div>
          <div class="lab-storage-body">
            <div class="lab-storage-section">
              <h3>Cookies ({cookies().length}) — Rust jar including HttpOnly</h3>
              <For each={cookies()} fallback={<p class="muted">(empty cookie jar)</p>}>
                {(cookie) => (
                  <div class="lab-cookie-row">
                    <span>
                      {cookie.name}={cookie.value}
                    </span>
                    <Show when={cookie.httpOnly}>
                      <span class="lab-pill">HttpOnly</span>
                    </Show>
                    <Show when={cookie.secure}>
                      <span class="lab-pill">Secure</span>
                    </Show>
                    <span class="muted">
                      {cookie.domain}
                      {cookie.path} · SameSite={cookie.sameSite}
                    </span>
                  </div>
                )}
              </For>
            </div>
            <div class="lab-storage-section">
              <h3>localStorage ({localEntries().length})</h3>
              <For each={localEntries()} fallback={<p class="muted">(empty)</p>}>
                {(entry) => (
                  <div class="lab-cookie-row">
                    {entry.key} = {entry.value}
                  </div>
                )}
              </For>
            </div>
            <div class="lab-storage-section">
              <h3>sessionStorage ({sessionEntries().length})</h3>
              <For each={sessionEntries()} fallback={<p class="muted">(empty)</p>}>
                {(entry) => (
                  <div class="lab-cookie-row">
                    {entry.key} = {entry.value}
                  </div>
                )}
              </For>
            </div>
          </div>
        </div>

        <div class="lab-panel" role="tabpanel" hidden={activeTab() !== "agent"}>
          <div class="lab-storage-bar">
            <span class="strong">
              {marks().length} interactive {marks().length === 1 ? "mark" : "marks"}
            </span>
            <button type="button" disabled={!ready()} onClick={refreshAgentMarks}>
              Scan page
            </button>
          </div>
          <div class="lab-marks-list">
            <For
              each={marks()}
              fallback={<p class="muted">No visible interactive elements in viewport.</p>}
            >
              {(mark) => (
                <button
                  type="button"
                  class="lab-mark-row"
                  onMouseEnter={() => {
                    hoverMarkIndex = mark.index
                    redrawPrimaryOverlay(null)
                  }}
                  onMouseLeave={() => {
                    hoverMarkIndex = null
                    redrawPrimaryOverlay(null)
                  }}
                  onClick={() => {
                    const active = getActiveSession()
                    if (!active) return
                    dispatchClickAt(
                      active,
                      { x: mark.rect.x + mark.rect.w / 2, y: mark.rect.y + mark.rect.h / 2 },
                      `agent clicked [${mark.index}] ${mark.label}`,
                    )
                  }}
                >
                  [{mark.index}] &lt;{mark.tag}&gt; {mark.label}
                </button>
              )}
            </For>
          </div>
          <pre class="demo-pre demo-pre-sm">{agentText()}</pre>
        </div>

        <div class="lab-panel" role="tabpanel" hidden={activeTab() !== "source"}>
          <div class="lab-storage-bar">
            <button
              type="button"
              disabled={!ready()}
              onClick={() => {
                const active = getActiveSession()
                if (!active) return
                setSourceHtml(safeRead(() => active.tab.eval("document.documentElement.outerHTML")))
              }}
            >
              Read live DOM
            </button>
            <button
              type="button"
              disabled={!ready()}
              onClick={() => {
                void openFixtureInActive(
                  `${window.location.origin}/api/demo/hello.txt`,
                  sourceHtml(),
                )
              }}
            >
              Mount HTML
            </button>
          </div>
          <textarea
            class="lab-textarea"
            rows={16}
            spellcheck={false}
            value={sourceHtml()}
            onInput={(e) => setSourceHtml(e.currentTarget.value)}
          />
        </div>

        <div class="lab-panel" role="tabpanel" hidden={activeTab() !== "lab"}>
          <div class="lab-control-row">
            <span class="lab-row-label">Layout</span>
            <div class="lab-group" role="group" aria-label="Layout">
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(stageMode() === "single")}
                onClick={() => changeStageMode("single")}
              >
                One tab
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(stageMode() === "dual")}
                onClick={() => changeStageMode("dual")}
              >
                Desktop + mobile
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(stageMode() === "split")}
                onClick={() => changeStageMode("split")}
              >
                Two sessions
              </button>
            </div>
          </div>

          <div class="lab-control-row">
            <span class="lab-row-label">Overlays</span>
            <div class="lab-group" role="group" aria-label="Overlays">
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(inspectEnabled())}
                onClick={() => {
                  const next = !inspectEnabled()
                  setInspectEnabled(next)
                  if (!next) pinnedInspect = null
                  redrawPrimaryOverlay(null)
                  setHint(
                    next
                      ? "Inspect mode on: hover the rendered frame to hit-test Blitz layout; click to pin."
                      : "Inspect mode off: clicks dispatch to the page.",
                  )
                }}
              >
                Element inspector
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(marksEnabled())}
                onClick={() => {
                  const next = !marksEnabled()
                  setMarksEnabled(next)
                  if (next) refreshAgentMarks()
                  else redrawPrimaryOverlay(null)
                }}
              >
                Agent marks
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(paintDiffEnabled())}
                onClick={() => {
                  const next = !paintDiffEnabled()
                  setPaintDiffEnabled(next)
                  setHint(
                    next
                      ? "Paint diff on: changed pixels flash on every re-render."
                      : "Paint diff off.",
                  )
                }}
              >
                Paint diff
              </button>
            </div>
          </div>

          <div class="lab-control-row">
            <label class="lab-row-label" for="vp-width">
              Viewport
            </label>
            <div class="lab-group lab-vp-controls" role="group" aria-label="Viewport size">
              <input
                id="vp-width"
                type="range"
                min={320}
                max={880}
                step={20}
                value={viewportWidth()}
                disabled={!ready()}
                aria-label="Viewport width"
                onInput={(e) => {
                  setViewportWidth(Number.parseInt(e.currentTarget.value, 10))
                  renderStage()
                }}
              />
              <span class="demo-label">
                {viewportWidth()}×{VIEWPORT_HEIGHT}
              </span>
              <button
                type="button"
                disabled={!ready()}
                onClick={() => {
                  const active = getActiveSession()
                  if (!active) return
                  dispatchInputTo(
                    active,
                    {
                      tag: "wheel",
                      val: {
                        x: viewportWidth() / 2,
                        y: VIEWPORT_HEIGHT / 2,
                        deltaX: 0,
                        deltaY: -240,
                      },
                    },
                    "scroll up",
                  )
                }}
              >
                Scroll up
              </button>
              <button
                type="button"
                disabled={!ready()}
                onClick={() => {
                  const active = getActiveSession()
                  if (!active) return
                  dispatchInputTo(
                    active,
                    {
                      tag: "wheel",
                      val: {
                        x: viewportWidth() / 2,
                        y: VIEWPORT_HEIGHT / 2,
                        deltaX: 0,
                        deltaY: 240,
                      },
                    },
                    "scroll down",
                  )
                }}
              >
                Scroll down
              </button>
            </div>
          </div>

          <div class="lab-control-row">
            <span class="lab-row-label">Event loop</span>
            <div class="lab-group" role="group" aria-label="Event loop">
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(loopPaused())}
                onClick={toggleLoopPaused}
              >
                {loopPaused() ? "Paused" : "Running"}
              </button>
              <button type="button" disabled={!ready()} onClick={stepOnce}>
                Step 1 tick
              </button>
            </div>
          </div>

          <div class="lab-control-row">
            <span class="lab-row-label">User agent</span>
            <div class="lab-group" role="group" aria-label="User agent">
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(uaPreset() === "tinybrowser")}
                onClick={() => applyUserAgent("tinybrowser")}
              >
                tinybrowser
              </button>
              <button
                type="button"
                disabled={!ready()}
                aria-pressed={ariaBool(uaPreset() === "chrome")}
                onClick={() => applyUserAgent("chrome")}
              >
                Chrome
              </button>
            </div>
          </div>
        </div>
      </aside>

      <details class="demo-how">
        <summary>How this works</summary>
        <p>
          This page runs tinybrowser compiled to WebAssembly. It parses HTML, lays out CSS, runs
          JavaScript in QuickJS, and writes RGBA pixels into the canvas element above.
        </p>
        <p>
          It is not an iframe. Pages that send <code>X-Frame-Options</code> or{" "}
          <code>frame-ancestors</code> headers still load. Because browser tabs cannot open raw TCP
          sockets, HTTP requests go through <code>/api/hop</code>.
        </p>
      </details>
    </>
  )
}
