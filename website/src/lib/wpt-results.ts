import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"

import { WPT_PUBLIC_DATA_DIR } from "./env"
import {
  RECORD_SCHEMA,
  type IndexEntry,
  type Regression,
  type RunRecord,
  type Snapshot,
} from "./record"

function read<T>(name: string): T | null {
  const path = join(WPT_PUBLIC_DATA_DIR, name)
  if (!existsSync(path)) return null
  return JSON.parse(readFileSync(path, "utf8")) as T
}

function readRecord(name: string): RunRecord | null {
  const record = read<RunRecord>(name)
  if (record !== null && record.schema !== RECORD_SCHEMA) {
    throw new Error(
      `${name}: record schema ${record.schema} is not ${RECORD_SCHEMA}; update record.ts with tools/wpt/cli.py`,
    )
  }
  return record
}

export function loadWptSnapshot(): Snapshot {
  return {
    latest: readRecord("latest.json"),
    index: read<Array<IndexEntry>>("index.json") ?? [],
    regressions: read<Regression>("regressions.json"),
  }
}
