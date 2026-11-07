/**
 * The WPT record, as published by `tools/wpt/cli.py publish`.
 *
 * Build-time only: the files are copied into public/wpt/data by
 * scripts/sync-data.ts, so the site is a static snapshot and the raw entries
 * stay fetchable at the same paths.
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

/** Passing subtests over every subtest that reported. */
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

export function formatDate(epoch: number | null): string {
  if (epoch === null) return "—"
  const date = new Date(epoch * 1000)
  if (Number.isNaN(date.getTime())) return "—"
  return date.toISOString().replace("T", " ").slice(0, 16) + " UTC"
}

export function shortSha(sha: string | null): string {
  return sha ? sha.slice(0, 8) : "—"
}
