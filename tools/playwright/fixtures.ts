import { test as base } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

export interface Daemon {
  /** HTTP discovery origin, e.g. `http://127.0.0.1:41234`. */
  origin: string;
  /** Classic-script + fetch page served by the fixture. */
  pageUrl: string;
  /** The same page reached through `localhost`, for cross-site navigations. */
  crossSitePageUrl: string;
  /** Cross-origin page with a secret body, for `javascript:` iframe src. */
  victimUrl: string;
  /** Same-origin page that mints a blob URL and posts it to the parent. */
  blobChildUrl: string;
  /** Page with a child frame and tall content, for viewport and clip checks. */
  frameUrl: string;
  /** Deterministic layout page for screenshot assertions. */
  shotUrl: string;
  /** Page styled only by an external sheet. */
  styledUrl: string;
  /** Page whose external sheet 404s. */
  brokenUrl: string;
  /** The same inline-styled box without any link element. */
  plainUrl: string;
  /** Page with a button and an input for click/type automation. */
  interactiveUrl: string;
  /** Page with GET and POST forms that submit to the echo route. */
  formUrl: string;
  /** Page with a realistic multipart form covering every control type. */
  richUrl: string;
  /** Gradient, shadow, image, text, and filter pages for Chromium pixel diffs. */
  gradientUrl: string;
  shadowUrl: string;
  imagedUrl: string;
  textUrl: string;
  filterUrl: string;
}

interface Fixtures {
  daemon: Daemon;
}

const PAGE = `<!doctype html><title>tiny</title><script src="/lib.js"></script><script>window.ready = false; fetch('/data').then(r => r.text()).then(t => { window.payload = t; window.ready = true; });</script>`;
const SECRET = `<!doctype html><title>secret</title><body>SECRET_VICTIM</body>`;
const BLOB_CHILD = `<!doctype html><script>
parent.postMessage(URL.createObjectURL(new Blob(['GONE'], { type: 'text/plain' })), '*');
</script>`;

/** A child frame plus tall content, for viewport and clip probes. */
const FRAMES = `<!doctype html><title>frames</title><div style="height:2000px"></div><iframe id="child" src="/frame"></iframe>`;
const CHILD = `<!doctype html><title>child</title><p>child</p>`;

/** Fixed colors and positions the screenshot spec probes by pixel. */
const SHOT = `<!doctype html><title>shot</title><style>
html, body { margin: 0; padding: 0; }
#red { background: #ff0000; width: 100px; height: 50px; }
#blue { background: #0000ff; width: 50px; height: 50px; }
p { margin: 16px 0 0 0; font-size: 20px; }
</style><div id="red"></div><div id="blue"></div><p>Hello screenshot</p>`;

/** Styled only by an external sheet, so the load event must wait for it. */
const STYLED = `<!doctype html><title>styled</title>
<link rel="stylesheet" href="/styles.css">
<div class="hot"></div>`;

/** A link that 404s must not hold the load event forever. */
const BROKEN = `<!doctype html><title>broken</title>
<link rel="stylesheet" href="/missing.css">
<div style="background:#123456;width:20px;height:20px"></div>`;

/** The same box without the link, to isolate the offset. */
const PLAIN = `<!doctype html><title>plain</title>
<div style="background:#123456;width:20px;height:20px"></div>`;

/** Interactive controls for click/type/selector automation. */
const INTERACTIVE = `<!doctype html><title>interactive</title>
<div id="go" style="width:80px;height:30px" onclick="document.getElementById('out').textContent = 'clicked'">Go</div>
<textarea id="name"></textarea>
<div id="out">idle</div>
<script>
document.getElementById('name').addEventListener('input', function (event) {
  document.getElementById('out').textContent = 'typed:' + event.target.value;
});
</script>`;

const STYLES = ".hot { background: #00ff00; width: 60px; height: 60px; }";

/** Paint-focused pages for Chromium pixel diffs. All margin-free and static. */
const GRADIENT = `<!doctype html><title>gradient</title><style>
html, body { margin: 0; padding: 0; }
div { width: 200px; height: 120px; background: linear-gradient(red, blue); }
</style><div></div>`;
const SHADOW = `<!doctype html><title>shadow</title><style>
html, body { margin: 0; padding: 40px; background: #ffffff; }
div { width: 160px; height: 100px; background: #3366cc; border-radius: 12px;
  box-shadow: 8px 10px 12px rgba(0, 0, 0, 0.5); }
</style><div></div>`;
const IMAGED = `<!doctype html><title>imaged</title><style>
html, body { margin: 0; padding: 0; }
</style><img src="/dot.png" width="100" height="100" style="display:block">`;
const TEXT = `<!doctype html><title>text</title><style>
html, body { margin: 0; padding: 16px; }
h1 { font-size: 32px; margin: 0 0 8px 0; }
p { font-size: 16px; margin: 0 0 8px 0; }
b { font-weight: bold; }
</style><h1>Hello pixels</h1><p>The quick brown fox jumps over the lazy dog. <b>Bold words stand out.</b></p><p>Second paragraph with more words to shape and rasterize.</p>`;
const FILTERED = `<!doctype html><title>filtered</title><style>
html, body { margin: 0; padding: 0; }
div { width: 120px; height: 80px; background: #cc3333; filter: blur(3px); }
</style><div></div>`;
/** 1x1 red PNG for the image page. */
const DOT_PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

const FORM = `<!doctype html><title>form</title>
<form id="get-form" method="get" action="/echo">
  <input id="g-name" name="name">
  <select id="g-color" name="color">
    <option value="red">Red</option>
    <option value="blue">Blue</option>
  </select>
  <input id="g-agree" type="checkbox" name="agree" value="yes">
  <button id="g-submit" type="submit">Send</button>
</form>
<form id="post-form" method="post" action="/echo">
  <input id="p-name" name="name">
  <button id="p-submit" type="submit">Post</button>
</form>`;

/** A realistic multipart form covering every control type Playwright can drive. */
const RICH_FORM = `<!doctype html><title>rich form</title>
<form id="rich-form" method="post" action="/echo" enctype="multipart/form-data">
  <input id="r-name" name="name" type="text">
  <input id="r-email" name="email" type="email">
  <input id="r-age" name="age" type="number">
  <input id="r-date" name="date" type="date">
  <textarea id="r-bio" name="bio"></textarea>
  <select id="r-color" name="color">
    <option value="red">Red</option>
    <option value="blue">Blue</option>
  </select>
  <input id="r-agree" type="checkbox" name="agree" value="yes">
  <input id="r-size-s" type="radio" name="size" value="s">
  <input id="r-size-l" type="radio" name="size" value="l">
  <input id="r-file" type="file" name="upload">
  <button id="r-submit" type="submit">Send</button>
</form>`;

async function waitForPort(jsonPath: string, timeoutMs: number): Promise<number> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      return JSON.parse(readFileSync(jsonPath, "utf8")).port as number;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
  }
  throw new Error(`daemon.json missing at ${jsonPath}`);
}

export const test = base.extend<Fixtures>({
  daemon: async ({}, use) => {
    const binary = process.env.TINYBROWSER_BINARY;
    if (!binary) {
      throw new Error("TINYBROWSER_BINARY is not set; run ./tools/playwright/run");
    }
    const root = mkdtempSync(join(tmpdir(), "tinybrowser-pw-"));
    const runtime = join(root, "run");
    const data = join(root, "data");
    mkdirSync(runtime, { recursive: true });
    mkdirSync(data, { recursive: true });

    const httpServer = createServer((request, response) => {
      const path = new URL(request.url ?? "/", "http://localhost").pathname;
      if (path === "/page") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(PAGE);
      } else if (path === "/secret") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(SECRET);
      } else if (path === "/blob-child") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(BLOB_CHILD);
      } else if (path === "/frames") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(FRAMES);
      } else if (path === "/frame") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(CHILD);
      } else if (path === "/shot") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(SHOT);
      } else if (path === "/styled") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(STYLED);
      } else if (path === "/broken") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(BROKEN);
      } else if (path === "/plain") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(PLAIN);
      } else if (path === "/interactive") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(INTERACTIVE);
      } else if (path === "/form") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(FORM);
      } else if (path === "/rich") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(RICH_FORM);
      } else if (path === "/gradient") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(GRADIENT);
      } else if (path === "/shadow") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(SHADOW);
      } else if (path === "/imaged") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(IMAGED);
      } else if (path === "/text") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(TEXT);
      } else if (path === "/filtered") {
        response.writeHead(200, { "content-type": "text/html" });
        response.end(FILTERED);
      } else if (path === "/dot.png") {
        response.writeHead(200, { "content-type": "image/png" });
        response.end(Buffer.from(DOT_PNG, "base64"));
      } else if (path === "/echo") {
        const chunks: Buffer[] = [];
        request.on("data", (chunk: Buffer) => chunks.push(chunk));
        request.on("end", () => {
          const body = Buffer.concat(chunks).toString("utf8");
          const text = request.method === "POST"
            ? `POST ${body}`
            : `GET ${new URL(request.url ?? "/", "http://localhost").search.replace(/^\?/, "")}`;
          response.writeHead(200, { "content-type": "text/html" });
          response.end(`<!doctype html><title>echo</title><pre id="result">${text}</pre>`);
        });
      } else if (path === "/styles.css") {
        // Delay the sheet so the load-delay spec discriminates: a browser
        // that fires load without waiting would screenshot white.
        setTimeout(() => {
          response.writeHead(200, { "content-type": "text/css" });
          response.end(STYLES);
        }, 300);
      } else if (path === "/lib.js") {
        response.writeHead(200, { "content-type": "text/javascript" });
        response.end("window.fromLib = 7;");
      } else if (path === "/data") {
        response.writeHead(200, { "content-type": "text/plain" });
        response.end("payload");
      } else {
        response.writeHead(404);
        response.end("not found");
      }
    });
    await new Promise<void>((resolve) => httpServer.listen(0, "::", resolve));
    const httpPort = (httpServer.address() as AddressInfo).port;

    // Detached so cleanup can kill the daemon and its renderer children as one
    // process group.
    const daemon = spawn(binary, ["serve", "--profile=default"], {
      env: { ...process.env, XDG_RUNTIME_DIR: runtime, XDG_DATA_HOME: data },
      stdio: "ignore",
      detached: true,
    });
    if (daemon.pid === undefined) throw new Error("daemon did not spawn");

    const port = await waitForPort(
      join(runtime, "tinybrowser", "default", "daemon.json"),
      10_000,
    );

    await use({
      origin: `http://127.0.0.1:${port}`,
      pageUrl: `http://127.0.0.1:${httpPort}/page`,
      crossSitePageUrl: `http://[::1]:${httpPort}/page`,
      victimUrl: `http://[::1]:${httpPort}/secret`,
      blobChildUrl: `http://127.0.0.1:${httpPort}/blob-child`,
      frameUrl: `http://127.0.0.1:${httpPort}/frames`,
      shotUrl: `http://127.0.0.1:${httpPort}/shot`,
      styledUrl: `http://127.0.0.1:${httpPort}/styled`,
      brokenUrl: `http://127.0.0.1:${httpPort}/broken`,
      plainUrl: `http://127.0.0.1:${httpPort}/plain`,
      interactiveUrl: `http://127.0.0.1:${httpPort}/interactive`,
      formUrl: `http://127.0.0.1:${httpPort}/form`,
      richUrl: `http://127.0.0.1:${httpPort}/rich`,
      gradientUrl: `http://127.0.0.1:${httpPort}/gradient`,
      shadowUrl: `http://127.0.0.1:${httpPort}/shadow`,
      imagedUrl: `http://127.0.0.1:${httpPort}/imaged`,
      textUrl: `http://127.0.0.1:${httpPort}/text`,
      filterUrl: `http://127.0.0.1:${httpPort}/filtered`,
    });

    try {
      process.kill(-daemon.pid, "SIGKILL");
    } catch {
      // Already gone.
    }
    await new Promise<void>((resolve) => httpServer.close(() => resolve()));
    rmSync(root, { recursive: true, force: true });
  },
});

export { expect } from "@playwright/test";
