/**
 * Populates `public/wpt/data` with the published WPT record, which is both
 * what `src/lib/wpt-results.ts` reads at build time and what the site serves
 * under `/wpt/data/*`.
 *
 * In CI (or when `WPT_RESULTS_DIR` points at a local checkout of
 * `tinybrowser-dev/tinybrowser-wpt-results`), the files are copied from disk.
 * When no local checkout exists, the JSON files the pages render are fetched
 * from the public GitHub repository so local dev renders the real dashboard
 * out of the box.
 */
import { copyFileSync, existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"

import { WPT_PUBLIC_DATA_DIR, WPT_RESULTS_DIR, WPT_RESULTS_RAW_BASE } from "../src/lib/env.ts"

const JSON_FILES = ["index.json", "latest.json", "regressions.json"]

// The failure list and README are shipped when copying a local checkout so the
// served `/wpt/data/*` directory is complete, but skipped on the remote dev
// fallback so `bun run dev` doesn't download the multi-megabyte gzip sidecar.
const EXTRA_FILES = ["failures.jsonl.gz", "README.md"]

rmSync(WPT_PUBLIC_DATA_DIR, { recursive: true, force: true })
mkdirSync(WPT_PUBLIC_DATA_DIR, { recursive: true })

let copied = 0
for (const name of [...JSON_FILES, ...EXTRA_FILES]) {
  const from = join(WPT_RESULTS_DIR, name)
  if (!existsSync(from)) continue
  copyFileSync(from, join(WPT_PUBLIC_DATA_DIR, name))
  copied += 1
}

if (copied > 0) {
  console.log(`sync-wpt-results: copied ${copied} files from ${WPT_RESULTS_DIR}`)
} else {
  let fetched = 0
  for (const name of JSON_FILES) {
    try {
      const response = await fetch(`${WPT_RESULTS_RAW_BASE}/${name}`)
      if (!response.ok) continue
      const bytes = new Uint8Array(await response.arrayBuffer())
      writeFileSync(join(WPT_PUBLIC_DATA_DIR, name), bytes)
      fetched += 1
    } catch {
      // Offline or unreachable: leave missing so the dashboard renders its empty state.
    }
  }
  if (fetched > 0) {
    console.log(`sync-wpt-results: fetched ${fetched} files from ${WPT_RESULTS_RAW_BASE}`)
  } else {
    console.warn(
      `sync-wpt-results: no record in ${WPT_RESULTS_DIR}; the dashboard will render its empty state`,
    )
  }
}
