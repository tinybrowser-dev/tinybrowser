import { readFileSync } from "node:fs"
import { parsePalettes, type Palette } from "../src/lib/palette.ts"

const CSS = "src/styles/themes.css"

function toRgb(hex: string): [number, number, number] {
  const match = /^#([0-9a-f]{6})$/i.exec(hex)
  if (!match) throw new Error(`Not a six digit hex colour: ${hex}`)

  return [
    Number.parseInt(match[1].slice(0, 2), 16),
    Number.parseInt(match[1].slice(2, 4), 16),
    Number.parseInt(match[1].slice(4, 6), 16),
  ]
}

function channel(value: number): number {
  const scaled = value / 255
  return scaled <= 0.040_45 ? scaled / 12.92 : ((scaled + 0.055) / 1.055) ** 2.4
}

function contrast(palette: Palette, foreground: string, background: string): number {
  const [red, green, blue] = toRgb(palette[foreground])
  const [otherRed, otherGreen, otherBlue] = toRgb(palette[background])

  const first = 0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
  const second =
    0.2126 * channel(otherRed) + 0.7152 * channel(otherGreen) + 0.0722 * channel(otherBlue)

  const lighter = Math.max(first, second)
  const darker = Math.min(first, second)
  return (lighter + 0.05) / (darker + 0.05)
}

const PAIRS: Array<{ label: string; fg: string; bg: string; min: number }> = [
  { label: "body text", fg: "--fg-0", bg: "--bg-0", min: 7 },
  { label: "meta text", fg: "--fg-1", bg: "--bg-0", min: 7 },
  { label: "code text", fg: "--fg-0", bg: "--bg-1", min: 7 },
  { label: "text on selection", fg: "--fg-0", bg: "--bg-2", min: 7 },
  { label: "meta on selection", fg: "--fg-1", bg: "--bg-2", min: 4.5 },
  { label: "hairline on page", fg: "--fg-2", bg: "--bg-0", min: 1.2 },
  { label: "hairline on surface", fg: "--fg-2", bg: "--bg-1", min: 1.2 },
  { label: "hairline on selection", fg: "--fg-2", bg: "--bg-2", min: 1.2 },
  // A bar segment is a graphic, not a hairline: WCAG 1.4.11 asks for 3:1 so a
  // failure stays visible against the empty track.
  { label: "bar fail on track", fg: "--fg-1", bg: "--bg-2", min: 3 },
  { label: "bar pass on track", fg: "--fg-0", bg: "--bg-2", min: 3 },
]

const { light, dark } = parsePalettes(readFileSync(CSS, "utf8"))
const failures: Array<string> = []

for (const [name, palette] of [
  ["light", light],
  ["dark", dark],
] as const) {
  for (const pair of PAIRS) {
    const ratio = contrast(palette, pair.fg, pair.bg)
    if (ratio < pair.min) {
      failures.push(
        `${name}: ${pair.label} is ${ratio.toFixed(2)}:1, needs ${pair.min}:1 (${pair.fg} on ${pair.bg})`,
      )
    }
  }
}

if (failures.length > 0) {
  console.error("\nPalette contrast check failed:\n")
  for (const failure of failures) console.error(`  ✗ ${failure}`)
  console.error("")
  process.exit(1)
}

console.log(`palette: ${CSS}`)
for (const [name, palette] of [
  ["light", light],
  ["dark", dark],
] as const) {
  const body = contrast(palette, "--fg-0", "--bg-0")
  const meta = contrast(palette, "--fg-1", "--bg-0")
  console.log(`  ${name.padEnd(5)} body ${body.toFixed(1)}:1  meta ${meta.toFixed(1)}:1`)
}
