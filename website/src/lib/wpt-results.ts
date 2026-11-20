import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"

import { WPT_PUBLIC_DATA_DIR } from "./env"
import type { IndexEntry, Regression, RunRecord, Snapshot } from "./record"

function read<T>(name: string): T | null {
  const path = join(WPT_PUBLIC_DATA_DIR, name)
  if (!existsSync(path)) return null
  return JSON.parse(readFileSync(path, "utf8")) as T
}

export function loadWptSnapshot(): Snapshot {
  return {
    latest: read<RunRecord>("latest.json"),
    index: read<Array<IndexEntry>>("index.json") ?? [],
    regressions: read<Regression>("regressions.json"),
  }
}
