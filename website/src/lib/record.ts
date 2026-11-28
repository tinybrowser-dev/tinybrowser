/**
 * The WPT record, as published by `tools/wpt/cli.py publish`.
 *
 * Build-time only: the files are copied into public/wpt/data by
 * scripts/sync-wpt-results.ts, so the site is a static snapshot and the raw
 * entries stay fetchable at the same paths.
 */

export type Counters = {
  tests: number
  pass: number
  expected_fail: number
  unexpected: number
  timeout: number
  crash: number
  error: number
  skip: number
  subtests: number
  subfail: number
  /**
   * Corpus subtests the run never reported: a worker variant that did not
   * run, or a file that stopped before testharness started. They count in
   * `subtests` and never in `subtest_pass`.
   */
  subtest_unreported: number
  time_ms: number
  [key: `subtest_${string}`]: number
}

export type Failure = {
  test: string
  subtest: string
  status: string
}

export type RunRecord = {
  schema: number
  product_revision: string | null
  wpt_revision: string | null
  time_start: number | null
  time_end: number | null
  totals: Counters
  areas: Record<string, Counters>
  /**
   * Only `merge` output carries the list. `publish` moves it into the gzipped
   * sidecar so the committed record stays small.
   */
  failures?: Array<Failure>
}

/** Mirrors `RECORD_SCHEMA` in `tools/wpt/cli.py`; a bump must update both. */
export const RECORD_SCHEMA = 2

export type IndexEntry = {
  sha: string | null
  date: string
  wpt_revision: string | null
  totals: Counters
}

export type Regression = {
  sha: string | null
  previous_sha: string | null
  wpt_revision: string | null
  comparable: boolean
  regressions: Array<Failure>
  new_timeouts: Array<Failure>
  fixes: Array<Failure>
}

export type Snapshot = {
  latest: RunRecord | null
  index: Array<IndexEntry>
  regressions: Regression | null
}

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
