/**
 * The WPT record helpers. Types and `RECORD_SCHEMA` are generated from the
 * single owner `tools/wpt-report/src/record.rs` into `./record-types`.
 */

export {
  RECORD_SCHEMA,
  type Counters,
  type Failure,
  type IndexEntry,
  type Regression,
  type RunRecord,
  type Snapshot,
} from "./record-types"

import type { Counters } from "./record-types"

export function counter(counters: Counters, key: string): number {
  const value = (counters as Record<string, number | undefined>)[key]
  return typeof value === "number" ? value : 0
}

/** Passing subtests over the scored corpus, including unreported subtests. */
export function subtestPercent(counters: Counters): number | null {
  const total = counter(counters, "subtests")
  if (total === 0) return null
  return (counter(counters, "subtest_pass") / total) * 100
}

export function testPercent(counters: Counters): number | null {
  const total = counter(counters, "tests")
  if (total === 0) return null
  return (counter(counters, "pass") / total) * 100
}

export function formatPercent(value: number | null): string {
  if (value === null) return "—"
  return `${value.toFixed(1)}%`
}

export function formatCount(value: number): string {
  return value.toLocaleString("en-US")
}

/** `1 failure`, `2 failures`, `0 failures`. */
export function plural(count: number, singular: string, pluralForm = `${singular}s`): string {
  return `${formatCount(count)} ${count === 1 ? singular : pluralForm}`
}

const shortDateFormatter = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  timeZone: "UTC",
})

export function formatShortDate(input: number | string | null): string {
  if (input === null) return "-"
  const date = typeof input === "number" ? new Date(input * 1000) : new Date(input)
  if (Number.isNaN(date.getTime())) return "-"
  return shortDateFormatter.format(date)
}

export function shortSha(sha: string | null): string {
  return sha ? sha.slice(0, 8) : "-"
}
