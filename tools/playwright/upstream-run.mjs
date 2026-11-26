#!/usr/bin/env node
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { spawn, spawnSync } from "node:child_process";
import http from "node:http";
import { cpus, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, "../..");
const PW_DIR = resolve(ROOT, ".playwright");
const TESTS_PATH = resolve(__dirname, "pw-tests.json");
const EXPECTATIONS_PATH = resolve(__dirname, "pw-expectations.json");

function parseArgs(argv) {
  const opts = { specs: [], update: false, jobs: Math.min(8, cpus().length || 4), timeout: null };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--update-expectations") {
      opts.update = true;
    } else if (argv[i] === "--spec" && argv[i + 1]) {
      opts.specs.push(argv[++i]);
    } else if ((argv[i] === "--jobs" || argv[i] === "-j") && argv[i + 1]) {
      opts.jobs = Math.max(1, Number(argv[++i]) || 1);
    } else if (argv[i] === "--timeout" && argv[i + 1]) {
      opts.timeout = Number(argv[++i]) || null;
    } else if (!argv[i].startsWith("-")) {
      opts.specs.push(argv[i]);
    } else {
      console.error(
        "usage: upstream-run.mjs [--spec <file>]... [--jobs <n>] [--timeout <ms>] [--update-expectations]",
      );
      process.exit(2);
    }
  }
  return opts;
}

function resolveBinary() {
  if (process.env.TINYBROWSER_BINARY) {
    return resolve(process.env.TINYBROWSER_BINARY);
  }
  const out = spawnSync(resolve(ROOT, "tools/binary-path"), ["--profile", "wpt"], {
    encoding: "utf8",
  });
  if (out.status === 0 && out.stdout.trim()) {
    const candidate = resolve(out.stdout.trim());
    if (existsSync(candidate)) {
      return candidate;
    }
  }
  return resolve(ROOT, "target/wpt/tinybrowser");
}

function waitForStatus(origin, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolvePromise, reject) => {
    const attempt = () => {
      http
        .get(`${origin}/status`, (res) => {
          res.resume();
          if (res.statusCode === 200) {
            resolvePromise();
          } else if (Date.now() < deadline) {
            setTimeout(attempt, 100);
          } else {
            reject(new Error(`status ${res.statusCode}`));
          }
        })
        .on("error", (err) => {
          if (Date.now() < deadline) {
            setTimeout(attempt, 100);
          } else {
            reject(err);
          }
        });
    };
    attempt();
  });
}

async function startDaemon(binary) {
  const runtimeDir = mkdtempSync(join(tmpdir(), "tb-pw-"));
  const daemonJson = join(runtimeDir, "tinybrowser", "default", "daemon.json");
  const child = spawn(binary, ["serve"], {
    env: {
      ...process.env,
      XDG_RUNTIME_DIR: runtimeDir,
      XDG_DATA_HOME: runtimeDir,
      XDG_STATE_HOME: runtimeDir,
      XDG_CACHE_HOME: runtimeDir,
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stderr = "";
  child.stderr.on("data", (chunk) => {
    stderr += chunk.toString();
  });

  const deadline = Date.now() + 10_000;
  let info = null;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      rmSync(runtimeDir, { recursive: true, force: true });
      throw new Error(`tinybrowser exited early (${child.exitCode}):\n${stderr}`);
    }
    if (existsSync(daemonJson)) {
      try {
        info = JSON.parse(readFileSync(daemonJson, "utf8"));
        break;
      } catch {
        // File write in progress.
      }
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  if (!info) {
    child.kill();
    rmSync(runtimeDir, { recursive: true, force: true });
    throw new Error(`timed out waiting for daemon.json:\n${stderr}`);
  }
  const origin = `http://${info.host}:${info.port}`;
  await waitForStatus(origin);
  return {
    origin,
    stop() {
      child.kill();
      rmSync(runtimeDir, { recursive: true, force: true });
    },
  };
}

function stripAnsi(text) {
  return String(text).replace(/\u001b\[[0-9;]*m/g, "");
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Expectations recorded as SKIP mean the outcome cannot be gated: Playwright
// itself skips the test, or the harness excludes it before the run so a timing
// race never turns into a pass. Excluded tests are reported as skipped and
// therefore never counted as passes.
function skippedTests(spec, expectations) {
  const prefix = `${spec}::`;
  return Object.entries(expectations)
    .filter(([id, entry]) => id.startsWith(prefix) && entry.expected === "SKIP")
    .map(([id]) => id.slice(prefix.length));
}

// Playwright's grep title joins the suite path with spaces, not " › ".
function grepPattern(title) {
  return escapeRegExp(title.split(" › ").join(" "));
}

function collectResults(report) {
  const results = {};
  const reasons = {};
  function walk(suite, parents) {
    const file = suite.file || parents[0] || "";
    const nextParents =
      suite.title && suite.title !== suite.file ? [...parents, suite.title] : parents;
    for (const spec of suite.specs ?? []) {
      const id = `${spec.file || file}::${[...nextParents, spec.title].join(" › ")}`;
      const test = spec.tests?.[0];
      const last = test?.results?.[test.results.length - 1];
      const status = last?.status ?? test?.status ?? "skipped";
      if (status === "passed") {
        results[id] = "PASS";
      } else if (status === "skipped") {
        results[id] = "SKIP";
        reasons[id] = "skipped";
      } else {
        results[id] = "FAIL";
        const msg = last?.error?.message || last?.errors?.[0]?.message || status;
        reasons[id] = stripAnsi(msg).split("\n")[0].slice(0, 160);
      }
    }
    for (const child of suite.suites ?? []) {
      walk(child, suite.file ? [] : nextParents);
    }
  }
  for (const suite of report.suites ?? []) {
    walk(suite, []);
  }
  return { results, reasons };
}

async function runOneSpec(binary, spec, timeout, traceRoot, portBase, skipped) {
  const reportFile = join(tmpdir(), `tb-pw-report-${process.pid}-${spec.replace(/[^a-z0-9]/gi, "_")}.json`);
  const traceDir = traceRoot ? join(traceRoot, spec.replace(/[^a-z0-9]/gi, "_")) : null;
  const daemon = await startDaemon(binary);
  try {
    const pwArgs = [
      "playwright",
      "test",
      "--config=tests/tinybrowser/playwright.config.ts",
      "--workers=1",
      ...(timeout ? [`--timeout=${timeout}`] : []),
      ...(skipped.length > 0
        ? [`--grep-invert=${skipped.map(grepPattern).join("|")}`]
        : []),
      spec,
    ];
    await new Promise((resolvePromise) => {
      const proc = spawn("npx", pwArgs, {
        cwd: PW_DIR,
        stdio: ["ignore", "pipe", "pipe"],
        env: {
          ...process.env,
          TINYBROWSER_CDP_ORIGIN: daemon.origin,
          PWTEST_JSON_OUTPUT: reportFile,
          PWTEST_PORT_BASE: String(portBase),
          ...(traceDir ? { PW_CDP_TRACE_DIR: traceDir } : {}),
        },
      });
      proc.on("close", () => resolvePromise());
      proc.on("error", () => resolvePromise());
    });
  } finally {
    daemon.stop();
  }

  if (!existsSync(reportFile)) {
    return { spec, results: {}, reasons: {}, missingReport: true };
  }
  const report = JSON.parse(readFileSync(reportFile, "utf8"));
  rmSync(reportFile, { force: true });
  const { results, reasons } = collectResults(report);
  const total = Object.keys(results).length;
  const pass = Object.values(results).filter((v) => v === "PASS").length;
  const fail = Object.values(results).filter((v) => v === "FAIL").length;
  const skip = Object.values(results).filter((v) => v === "SKIP").length;
  console.log(`  ${spec}: ${pass}/${total} passed (${fail} fail, ${skip} skip)`);
  return { spec, results, reasons, missingReport: false };
}

// Reads the per-test CDP traces the fixture dumped and ranks where failed
// tests spent their wall time, so the next missing feature is obvious.
function aggregateCdpTrace(traceRoot) {
  if (!existsSync(traceRoot)) {
    return;
  }
  const perMethod = new Map();
  const inflight = new Map();
  const events = new Map();
  let failedTests = 0;
  for (const spec of readdirSync(traceRoot)) {
    const specDir = join(traceRoot, spec);
    if (!existsSync(specDir)) {
      continue;
    }
    for (const file of readdirSync(specDir)) {
      const lines = readFileSync(join(specDir, file), "utf8").split("\n").filter(Boolean);
      for (const line of lines) {
        let record;
        try {
          record = JSON.parse(line);
        } catch {
          continue;
        }
        if (record.status === "passed") {
          continue;
        }
        failedTests += 1;
        const methods = new Set();
        for (const send of record.sends ?? []) {
          methods.add(send.method);
          const entry = perMethod.get(send.method) ?? { ms: 0, calls: 0, tests: 0 };
          entry.ms += send.ms;
          entry.calls += 1;
          perMethod.set(send.method, entry);
        }
        for (const method of methods) {
          perMethod.get(method).tests += 1;
        }
        for (const pending of record.inflight ?? []) {
          const entry = inflight.get(pending.method) ?? { count: 0, ms: 0 };
          entry.count += 1;
          entry.ms += pending.ms;
          inflight.set(pending.method, entry);
        }
        for (const event of record.events ?? []) {
          events.set(event, (events.get(event) ?? 0) + 1);
        }
      }
    }
  }
  if (failedTests === 0) {
    console.log("[pw-cdp] no failed tests carried CDP traces");
    return;
  }
  const seconds = (ms) => `${(ms / 1000).toFixed(1)}s`;
  const ranked = [...perMethod.entries()].sort((a, b) => b[1].ms - a[1].ms);
  const total = ranked.reduce((sum, [, entry]) => sum + entry.ms, 0);
  console.log(`\n[pw-cdp] ${failedTests} failed tests spent ${seconds(total)} in CDP commands, by method:`);
  for (const [method, entry] of ranked.slice(0, 10)) {
    const average = entry.calls ? Math.round(entry.ms / entry.calls) : 0;
    console.log(
      `  ${method.padEnd(32)} ${seconds(entry.ms).padStart(7)}  (${entry.calls} calls, ${entry.tests} tests, avg ${average}ms)`,
    );
  }
  if (inflight.size > 0) {
    console.log("[pw-cdp] commands still pending when the test failed:");
    for (const [method, entry] of [...inflight.entries()].sort((a, b) => b[1].ms - a[1].ms)) {
      const average = entry.count ? Math.round(entry.ms / entry.count) : 0;
      console.log(`  ${method.padEnd(32)} x${entry.count} (avg ${average}ms without a reply)`);
    }
  }
  if (events.size > 0) {
    const summary = [...events.entries()].sort((a, b) => b[1] - a[1]).slice(0, 8);
    console.log(`[pw-cdp] events in failed tests: ${summary.map(([name, count]) => `${name} x${count}`).join(", ")}`);
  }
}

async function main() {  const opts = parseArgs(process.argv.slice(2));
  if (!existsSync(PW_DIR)) {
    console.error("[pw-run] .playwright/ not found; run: node tools/playwright/upstream-setup.mjs");
    process.exit(1);
  }
  const binary = resolveBinary();
  if (!existsSync(binary)) {
    console.error(`[pw-run] tinybrowser binary not found at ${binary}`);
    console.error("[pw-run] build it with: cargo build --profile wpt -p tinybrowser");
    process.exit(1);
  }

  const allSpecs = JSON.parse(readFileSync(TESTS_PATH, "utf8"));
  const specs = opts.specs.length > 0 ? opts.specs : allSpecs;

  const results = {};
  const reasons = {};
  const traceRoot = mkdtempSync(join(tmpdir(), "tb-pw-cdp-"));
  const existing = existsSync(EXPECTATIONS_PATH)
    ? JSON.parse(readFileSync(EXPECTATIONS_PATH, "utf8"))
    : {};
  let index = 0;
  const workerCount = Math.min(opts.jobs, specs.length);
  await Promise.all(
    Array.from({ length: workerCount }, async () => {
      while (index < specs.length) {
        const spec = specs[index++];
        // 400 ports per run: worker restarts bump workerIndex past 0, so the
        // slot must absorb them without drifting into another run's range.
        const portBase = 10000 + (index % 56) * 400;
        const skipped = skippedTests(spec, existing);
        const outcome = await runOneSpec(
          binary,
          spec,
          opts.timeout,
          traceRoot,
          portBase,
          skipped,
        );
        Object.assign(results, outcome.results);
        Object.assign(reasons, outcome.reasons);
        for (const title of skipped) {
          const id = `${spec}::${title}`;
          if (!(id in results)) {
            results[id] = "SKIP";
            reasons[id] = "skipped";
          }
        }
      }
    }),
  );
  aggregateCdpTrace(traceRoot);

  if (opts.update) {
    const next = opts.specs.length > 0 ? { ...existing } : {};
    for (const id of Object.keys(results).sort()) {
      if (results[id] === "PASS") {
        delete next[id];
      } else if (results[id] === "SKIP" && existing[id]?.expected === "SKIP") {
        next[id] = existing[id];
      } else {
        next[id] = { expected: results[id], reason: reasons[id] };
      }
    }
    const sorted = Object.fromEntries(Object.keys(next).sort().map((k) => [k, next[k]]));
    writeFileSync(EXPECTATIONS_PATH, `${JSON.stringify(sorted, null, 2)}\n`);
  }

  const expectations = existsSync(EXPECTATIONS_PATH)
    ? JSON.parse(readFileSync(EXPECTATIONS_PATH, "utf8"))
    : {};

  let passed = 0;
  let expectedNonPass = 0;
  const regressions = [];
  const unexpectedPasses = [];

  for (const id of Object.keys(results).sort()) {
    const actual = results[id];
    const expected = expectations[id]?.expected ?? "PASS";
    if (actual === expected) {
      if (actual === "PASS") passed++;
      else expectedNonPass++;
    } else if (actual === "PASS") {
      unexpectedPasses.push({ id, expected });
    } else {
      regressions.push({ id, expected, actual, reason: reasons[id] });
    }
  }

  console.log(
    `\n[pw-gate] total=${Object.keys(results).length} pass=${passed} expected-non-pass=${expectedNonPass} regressions=${regressions.length} unexpected-pass=${unexpectedPasses.length}`,
  );
  for (const r of regressions) {
    console.error(`  REGRESSION ${r.id}: expected ${r.expected}, got ${r.actual} (${r.reason})`);
  }
  for (const u of unexpectedPasses) {
    console.error(
      `  UNEXPECTED PASS ${u.id}: expected ${u.expected}, got PASS (run --update-expectations)`,
    );
  }
  if (!opts.update && (regressions.length > 0 || unexpectedPasses.length > 0)) {
    process.exit(1);
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
