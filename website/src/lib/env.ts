/**
 * Build-time environment knobs and artifact paths for the website.
 *
 * Every script and build-time module reads its paths here so there is a single
 * source of truth for environment variables and directory layout:
 *
 * - `WPT_RESULTS_DIR`: path to a local checkout of
 *   `tinybrowser-dev/tinybrowser-wpt-results`. Defaults to `../wpt-results`
 *   (where CI checks it out). When absent locally, `scripts/sync-wpt-results.ts`
 *   fetches the published JSON files from `WPT_RESULTS_RAW_BASE` into
 *   `WPT_PUBLIC_DATA_DIR`.
 * - `TINYBROWSER_WASM_PATH`: path to the compiled `tinybrowser_wasm.wasm`
 *   WASI Preview 2 component. Defaults to
 *   `../target/wasm32-wasip2/release/tinybrowser_wasm.wasm`.
 * - `CI`: when truthy, `scripts/build-wasm.ts` fails hard if the Wasm
 *   component is missing instead of skipping.
 */
import { existsSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

function findWebsiteDir(start: string): string {
  let dir = start
  while (!existsSync(join(dir, "astro.config.mjs"))) {
    const parent = dirname(dir)
    if (parent === dir) {
      throw new Error(`Could not locate astro.config.mjs from ${start}`)
    }
    dir = parent
  }
  return dir
}

export const WEBSITE_DIR = findWebsiteDir(dirname(fileURLToPath(import.meta.url)))
export const REPO_DIR = resolve(WEBSITE_DIR, "..")

export const WPT_RESULTS_DIR = resolve(WEBSITE_DIR, process.env.WPT_RESULTS_DIR ?? "../wpt-results")
export const WPT_PUBLIC_DATA_DIR = join(WEBSITE_DIR, "public/wpt/data")
export const WPT_RESULTS_RAW_BASE =
  "https://raw.githubusercontent.com/tinybrowser-dev/tinybrowser-wpt-results/main"

export const TINYBROWSER_WASM_PATH = resolve(
  WEBSITE_DIR,
  process.env.TINYBROWSER_WASM_PATH ??
    join(REPO_DIR, "target/wasm32-wasip2/release/tinybrowser_wasm.wasm"),
)
export const WASM_PUBLIC_DIR = join(WEBSITE_DIR, "public/wasm")

export const IS_CI = Boolean(process.env.CI)
