/**
 * Canvas overlay helpers for the tinybrowser Engine Lab:
 * - Inspect: hover hit-test via QuickJS `elementFromPoint` + `getComputedStyle`
 * - Agent Marks (Set-of-Mark): interactive element extraction + numbered rects
 * - Paint Diff: RGBA framebuffer diff highlighting changed pixels
 */

export interface InspectHit {
  tag: string
  id: string
  className: string
  text: string
  rect: { x: number; y: number; w: number; h: number }
  font: string
  color: string
  bg: string
  display: string
}

export interface AgentMark {
  index: number
  tag: string
  role: string
  label: string
  href: string
  rect: { x: number; y: number; w: number; h: number }
}

export interface PaintDiffSummary {
  changedPixels: number
  totalPixels: number
  percent: number
}

export function inspectElementAt(
  evalJs: (source: string) => string,
  x: number,
  y: number,
): InspectHit | null {
  const rx = Math.round(x)
  const ry = Math.round(y)
  const script = `(() => {
    const el = document.elementFromPoint(${rx}, ${ry});
    if (!el) return "";
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    return JSON.stringify({
      tag: el.tagName.toLowerCase(),
      id: el.id || "",
      className: typeof el.className === "string" ? el.className.trim().slice(0, 60) : "",
      text: (el.textContent || "").replace(/\\s+/g, " ").trim().slice(0, 60),
      rect: { x: r.left, y: r.top, w: r.width, h: r.height },
      font: cs.fontSize + " " + (cs.fontFamily || "").split(",")[0].trim(),
      color: cs.color || "",
      bg: cs.backgroundColor || "",
      display: cs.display || ""
    });
  })()`
  try {
    const raw = evalJs(script)
    if (!raw) return null
    return JSON.parse(raw) as InspectHit
  } catch {
    return null
  }
}

export function collectAgentMarks(
  evalJs: (source: string) => string,
  viewport: { width: number; height: number },
): Array<AgentMark> {
  const script = `(() => {
    const nodes = document.querySelectorAll('a[href], button, input, select, textarea, [role="button"]');
    const out = [];
    for (let i = 0; i < nodes.length && out.length < 40; i += 1) {
      const el = nodes[i];
      const r = el.getBoundingClientRect();
      if (r.width < 4 || r.height < 4) continue;
      if (r.bottom <= 0 || r.top >= ${viewport.height} || r.right <= 0 || r.left >= ${viewport.width}) continue;
      const tag = el.tagName.toLowerCase();
      const rawLabel = el.getAttribute("aria-label") || el.value || el.placeholder || el.textContent || "";
      const label = String(rawLabel).replace(/\\s+/g, " ").trim().slice(0, 48);
      out.push({
        index: out.length + 1,
        tag,
        role: el.getAttribute("role") || (tag === "a" ? "link" : tag),
        label: label || ("<" + tag + ">"),
        href: tag === "a" ? (el.href || "") : "",
        rect: { x: r.left, y: r.top, w: r.width, h: r.height }
      });
    }
    return JSON.stringify(out);
  })()`
  try {
    const raw = evalJs(script)
    if (!raw) return []
    return JSON.parse(raw) as Array<AgentMark>
  } catch {
    return []
  }
}

export function drawInspectOverlay(
  canvas: HTMLCanvasElement,
  hit: InspectHit | null,
  pinned: boolean,
): void {
  const ctx = canvas.getContext("2d")
  if (!ctx) return
  if (!hit) return

  const { x, y, w, h } = hit.rect
  ctx.save()
  ctx.fillStyle = pinned ? "rgba(255, 90, 31, 0.22)" : "rgba(37, 99, 235, 0.18)"
  ctx.strokeStyle = pinned ? "#ff5a1f" : "#2563eb"
  ctx.lineWidth = 1.5
  ctx.fillRect(x, y, w, h)
  ctx.strokeRect(x + 0.5, y + 0.5, Math.max(0, w - 1), Math.max(0, h - 1))

  const idPart = hit.id ? `#${hit.id}` : ""
  const classPart = hit.className ? `.${hit.className.split(/\s+/).slice(0, 2).join(".")}` : ""
  const label = `${hit.tag}${idPart}${classPart} · ${Math.round(w)}×${Math.round(h)} · ${hit.font}`
  ctx.font = "600 11px ui-monospace, SFMono-Regular, Menlo, monospace"
  const metrics = ctx.measureText(label)
  const padX = 6
  const boxW = Math.min(canvas.width - 8, metrics.width + padX * 2)
  const boxH = 18
  const tipX = Math.max(4, Math.min(x, canvas.width - boxW - 4))
  const tipY = y >= boxH + 6 ? y - boxH - 4 : Math.min(canvas.height - boxH - 4, y + h + 4)

  ctx.fillStyle = "#111111"
  ctx.fillRect(tipX, tipY, boxW, boxH)
  ctx.fillStyle = "#fcfcfc"
  ctx.textBaseline = "middle"
  ctx.fillText(label, tipX + padX, tipY + boxH / 2, boxW - padX * 2)
  ctx.restore()
}

export function drawAgentMarksOverlay(
  canvas: HTMLCanvasElement,
  marks: Array<AgentMark>,
  activeIndex: number | null,
): void {
  const ctx = canvas.getContext("2d")
  if (!ctx) return
  ctx.save()
  ctx.font = "700 10px ui-monospace, SFMono-Regular, Menlo, monospace"
  ctx.textBaseline = "middle"

  for (const mark of marks) {
    const isActive = mark.index === activeIndex
    const { x, y, w, h } = mark.rect
    ctx.strokeStyle = isActive ? "#ff5a1f" : "#111111"
    ctx.lineWidth = isActive ? 2 : 1
    ctx.strokeRect(x + 0.5, y + 0.5, Math.max(0, w - 1), Math.max(0, h - 1))

    const badge = String(mark.index)
    const badgeW = Math.max(16, ctx.measureText(badge).width + 8)
    const badgeH = 14
    const bx = Math.max(0, Math.min(x, canvas.width - badgeW))
    const by = Math.max(0, y - badgeH + 2)

    ctx.fillStyle = isActive ? "#ff5a1f" : "#111111"
    ctx.fillRect(bx, by, badgeW, badgeH)
    ctx.fillStyle = "#fcfcfc"
    ctx.fillText(badge, bx + 4, by + badgeH / 2)
  }
  ctx.restore()
}

export function drawPaintDiffOverlay(
  canvas: HTMLCanvasElement,
  previous: Uint8Array,
  current: Uint8Array,
): PaintDiffSummary | null {
  if (previous.byteLength !== current.byteLength || current.byteLength === 0) {
    return null
  }
  const ctx = canvas.getContext("2d")
  if (!ctx) return null
  const width = canvas.width
  const height = canvas.height
  if (width * height * 4 !== current.byteLength) return null

  const overlay = ctx.createImageData(width, height)
  const dst = overlay.data
  let changedPixels = 0
  const totalPixels = width * height

  for (let offset = 0; offset < current.byteLength; offset += 4) {
    const dr = Math.abs((current[offset] ?? 0) - (previous[offset] ?? 0))
    const dg = Math.abs((current[offset + 1] ?? 0) - (previous[offset + 1] ?? 0))
    const db = Math.abs((current[offset + 2] ?? 0) - (previous[offset + 2] ?? 0))
    if (dr + dg + db > 12) {
      changedPixels += 1
      dst[offset] = 255
      dst[offset + 1] = 90
      dst[offset + 2] = 31
      dst[offset + 3] = 115
    }
  }

  if (changedPixels > 0) {
    ctx.putImageData(overlay, 0, 0)
  }
  const percent = totalPixels > 0 ? (changedPixels / totalPixels) * 100 : 0
  return { changedPixels, totalPixels, percent }
}
