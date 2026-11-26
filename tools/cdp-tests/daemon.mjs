import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export async function startDaemon(binary) {
  const root = mkdtempSync(join(tmpdir(), "tinybrowser-cdp-"));
  const runtime = join(root, "run");
  const data = join(root, "data");
  mkdirSync(runtime, { recursive: true });
  mkdirSync(data, { recursive: true });

  const child = spawn(binary, ["serve", "--profile=default"], {
    env: { ...globalThis.process.env, XDG_RUNTIME_DIR: runtime, XDG_DATA_HOME: data },
    stdio: "ignore",
  });
  if (child.pid === undefined) throw new Error("daemon did not spawn");
  let spawnError;
  child.once("error", (error) => {
    spawnError = error;
  });
  const closed = new Promise((resolve) => child.once("close", resolve));

  async function stop() {
    try {
      child.kill("SIGKILL");
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
    if (child.exitCode === null && child.signalCode === null) {
      await Promise.race([
        closed,
        new Promise((resolve) => setTimeout(resolve, 1_000)),
      ]);
    }
    rmSync(root, { recursive: true, force: true });
  }

  const registration = join(runtime, "tinybrowser", "default", "daemon.json");
  const deadline = Date.now() + 10_000;
  try {
    while (Date.now() < deadline) {
      if (spawnError) throw spawnError;
      if (child.exitCode !== null || child.signalCode !== null) {
        throw new Error("daemon exited before registration");
      }
      try {
        const port = JSON.parse(readFileSync(registration, "utf8")).port;
        if (!Number.isInteger(port) || port < 1 || port > 65_535) {
          throw new Error(`invalid daemon port: ${port}`);
        }
        return {
          origin: `http://127.0.0.1:${port}`,
          stop,
        };
      } catch (error) {
        if (error.code !== "ENOENT" && !(error instanceof SyntaxError)) {
          throw error;
        }
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
    }
    throw new Error(`daemon registration missing at ${registration}`);
  } catch (error) {
    await stop();
    throw error;
  }
}
