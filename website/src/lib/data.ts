import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"

import type { IndexEntry, Regression, RunRecord, Snapshot } from "./record"

/**
 * Where the results repo is checked out. Defaults to `data/` at the repo root.
 * CI overrides it when the checkout is somewhere else.
 */
const dir = process.env.WPT_DATA_DIR ?? "../data"

function read<T>(name: string): T | null {
  const path = join(dir, name)
  if (!existsSync(path)) return null
  return JSON.parse(readFileSync(path, "utf8")) as T
}

export function loadSnapshot(): Snapshot {
  return {
    latest: read<RunRecord>("latest.json"),
    index: read<Array<IndexEntry>>("index.json") ?? [],
    regressions: read<Regression>("regressions.json"),
  }
}

export const dataDir = dir
