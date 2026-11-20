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
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const here = dirname(fileURLToPath(import.meta.url))
const websiteDir = resolve(here, "..")
const repoDir = resolve(websiteDir, "..")
const component =
  process.env.TINYBROWSER_WASM_PATH ??
  join(repoDir, "target/wasm32-wasip2/release/tinybrowser_wasm.wasm")
const outDir = join(websiteDir, "public/wasm")
const shimSrc = join(websiteDir, "node_modules/@bytecodealliance/preview2-shim/dist/browser")
const jcoBin = join(websiteDir, "node_modules/.bin/jco")

if (!existsSync(component)) {
  if (process.env.CI) {
    console.error(`build-wasm: missing ${component} in CI`)
    process.exit(1)
  }
  console.warn(`build-wasm: missing ${component}; skipping Wasm bundle for /demo`)
  console.warn("  build with: cargo build -p tinybrowser-wasm --target wasm32-wasip2 --release")
  process.exit(0)
}

rmSync(outDir, { recursive: true, force: true })
mkdirSync(outDir, { recursive: true })

execFileSync(
  jcoBin,
  [
    "transpile",
    component,
    "-o",
    outDir,
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
    join(websiteDir, "src/demo/host.ts"),
    "--outfile",
    join(outDir, "host.js"),
    "--format",
    "esm",
    "--target",
    "browser",
  ],
  { stdio: "inherit" },
)

cpSync(shimSrc, join(outDir, "shims"), { recursive: true })

const wasmSize = statSync(join(outDir, "tinybrowser.core.wasm")).size
console.log(
  `build-wasm: ready in public/wasm/ (core wasm ${(wasmSize / 1024 / 1024).toFixed(2)} MiB)`,
)
