// Builds the demo bundle: transpiles the release component into
// `generated/`, where the browser page imports it.
//
// The component itself is built with cargo:
//   cargo build -p tinybrowser-wasm --target wasm32-wasip2 --release

import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");
const component = join(repo, "target/wasm32-wasip2/release/tinybrowser_wasm.wasm");
const out = join(here, "generated");

if (!existsSync(component)) {
  console.error(`missing ${component}`);
  console.error(
    "build it first: cargo build -p tinybrowser-wasm --target wasm32-wasip2 --release",
  );
  process.exit(1);
}

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
execFileSync(
  join(here, "node_modules/.bin/jco"),
  [
    "transpile",
    component,
    "-o",
    out,
    "--name",
    "tinybrowser",
    // The host contract is this demo's own module, so it needs no import map.
    "--map",
    "tinybrowser:browser/host=../host.mjs",
  ],
  { stdio: "inherit" },
);
