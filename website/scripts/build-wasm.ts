/**
 * Transpiles the compiled `tinybrowser-wasm` component into `public/wasm/`,
 * bundles the host driver (`src/demo/host.ts`), and copies the browser WASI
 * shims so `/demo` works identically in local dev and Cloudflare production.
 *
 * When `tinybrowser_wasm.wasm` has not been built yet, the script warns and
 * exits 0 so `bun run check` still succeeds without a Rust toolchain.
 */
import { execFileSync } from "node:child_process"
import { cpSync, existsSync, mkdirSync, rmSync, statSync } from "node:fs"
import { join } from "node:path"

import { IS_CI, TINYBROWSER_WASM_PATH, WASM_PUBLIC_DIR, WEBSITE_DIR } from "../src/lib/env.ts"

const shimSrc = join(WEBSITE_DIR, "node_modules/@bytecodealliance/preview2-shim/dist/browser")
const jcoBin = join(WEBSITE_DIR, "node_modules/.bin/jco")

if (!existsSync(TINYBROWSER_WASM_PATH)) {
  if (IS_CI) {
    console.error(`build-wasm: missing ${TINYBROWSER_WASM_PATH} in CI`)
    process.exit(1)
  }
  console.warn(`build-wasm: missing ${TINYBROWSER_WASM_PATH}; skipping Wasm bundle for /demo`)
  console.warn("  build with: cargo build -p tinybrowser-wasm --target wasm32-wasip2 --release")
  process.exit(0)
}

rmSync(WASM_PUBLIC_DIR, { recursive: true, force: true })
mkdirSync(WASM_PUBLIC_DIR, { recursive: true })

execFileSync(
  jcoBin,
  [
    "transpile",
    TINYBROWSER_WASM_PATH,
    "-o",
    WASM_PUBLIC_DIR,
    "--name",
    "tinybrowser",
    "--map",
    "tinybrowser:browser/host=./host.js",
  ],
  { stdio: "inherit" },
)

execFileSync(
  "bun",
  [
    "build",
    join(WEBSITE_DIR, "src/demo/host.ts"),
    "--outfile",
    join(WASM_PUBLIC_DIR, "host.js"),
    "--format",
    "esm",
    "--target",
    "browser",
  ],
  { stdio: "inherit" },
)

cpSync(shimSrc, join(WASM_PUBLIC_DIR, "shims"), { recursive: true })

const wasmSize = statSync(join(WASM_PUBLIC_DIR, "tinybrowser.core.wasm")).size
console.log(
  `build-wasm: ready in public/wasm/ (core wasm ${(wasmSize / 1024 / 1024).toFixed(2)} MiB)`,
)
