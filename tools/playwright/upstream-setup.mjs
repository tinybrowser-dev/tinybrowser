#!/usr/bin/env node
import { existsSync, readdirSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, "../..");
const PW_DIR = resolve(ROOT, ".playwright");
const PATCH_DIR = resolve(__dirname, "patches");
const PW_REPO = "https://github.com/microsoft/playwright.git";
const PW_TAG = "v1.63.0";

const args = process.argv.slice(2);
const force = args.includes("--force");
const skipBuild = args.includes("--skip-build");

function run(cmd, cmdArgs, cwd = ROOT) {
  const result = spawnSync(cmd, cmdArgs, {
    stdio: "inherit",
    cwd,
    env: { ...process.env, NPM_TOKEN: process.env.NPM_TOKEN ?? "" },
  });
  if (result.status !== 0) {
    console.error(`[pw-setup] ${cmd} ${cmdArgs.join(" ")} failed (code ${result.status})`);
    process.exit(result.status ?? 1);
  }
}

if (force && existsSync(PW_DIR)) {
  rmSync(PW_DIR, { recursive: true, force: true });
}

if (existsSync(PW_DIR)) {
  console.log("[pw-setup] .playwright/ already exists (use --force to redo)");
  process.exit(0);
}

console.log(`[pw-setup] cloning ${PW_REPO} @ ${PW_TAG} -> .playwright/`);
run("git", ["clone", "--depth=1", "--branch", PW_TAG, "--single-branch", PW_REPO, PW_DIR]);

const patches = existsSync(PATCH_DIR)
  ? readdirSync(PATCH_DIR)
      .filter((file) => file.endsWith(".patch"))
      .sort()
  : [];
for (const patch of patches) {
  console.log(`[pw-setup] applying ${patch}`);
  run("git", ["apply", "--whitespace=nowarn", join(PATCH_DIR, patch)], PW_DIR);
}

if (skipBuild) {
  process.exit(0);
}

console.log("[pw-setup] npm ci");
run("npm", ["ci"], PW_DIR);

console.log("[pw-setup] npm run build");
run("npm", ["run", "build"], PW_DIR);

console.log(`[pw-setup] ready in .playwright/ (${PW_TAG})`);
