import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { closeSync, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { join, resolve } from "node:path";
import { sandboxSample } from "./linux-sandbox.mjs";

const usage = "Usage: node tools/desktop/smoke.mjs <desktop-bundle> <linux-electron> [output-dir]";
const guestTitle = "OpenShell Native Browser Probe OK";
const entry = "/openshell-native-probe";
const fixture = {
  [entry]: ["text/html", '<!doctype html><title>OpenShell Native Browser Probe</title><script src="/probe.js"></script>'],
  "/probe.js": ["text/javascript", `if (typeof require === "undefined" && typeof window.openshellElectron === "undefined") {
    const worker = new Worker("/worker.js");
    worker.onmessage = (event) => {
      if (event.data === "openshell-native") document.title = ${JSON.stringify(guestTitle)};
      worker.terminate();
    };
    worker.postMessage("openshell-native");
  }`],
  "/worker.js": ["text/javascript", "self.onmessage = (event) => self.postMessage(event.data);"],
};

async function smoke(bundleArg, electronArg, outputArg) {
  assert.equal(process.platform, "linux", "Use a Linux graphical session");
  assert.ok(process.getuid() !== 0, "Run as an unprivileged session user");
  assert.ok(process.env.DISPLAY, "Start an X11 session first");

  const bundle = resolve(bundleArg);
  const electron = resolve(electronArg);
  const app = join(bundle, "app");
  const hostd = join(bundle, "hostd", "openshell-hostd.mjs");
  const ui = join(bundle, "ui");
  for (const file of [join(app, "main.cjs"), hostd, join(ui, "index.html"), electron]) {
    assert.ok(existsSync(file), `Missing Desktop input: ${file}`);
  }

  const outputRoot = resolve(outputArg ?? "out");
  mkdirSync(outputRoot, { recursive: true });
  const output = mkdtempSync(join(outputRoot, "desktop-"));
  const scratch = mkdtempSync("/tmp/openshell-native-desktop-");
  const report = { ok: false, output, sandbox: [] };
  console.log(`Artifacts: ${output}`);
  const server = createServer((request, response) => {
    const resource = fixture[request.url];
    if (request.method !== "GET" || !Array.isArray(resource)) {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, {
      "Content-Type": resource[0],
      "Content-Security-Policy": "default-src 'none'; script-src 'self'; worker-src 'self'",
      "X-Content-Type-Options": "nosniff",
    }).end(resource[1]);
  });
  let log;
  let child;
  let poll;
  let timeout;
  let timedOut = false;
  const stop = () => {
    if (child?.pid && child.exitCode === null && child.signalCode === null) {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch (error) {
        if (error.code !== "ESRCH") console.error(error.message);
      }
    }
  };

  try {
    const listening = once(server, "listening", { signal: AbortSignal.timeout(8_000) });
    server.listen(0, "127.0.0.1");
    await listening;
    const url = `http://127.0.0.1:${server.address().port}${entry}`;
    const node = join(bundle, "node", "bin", "node");
    const xauth = process.env.XAUTHORITY ?? join(process.env.HOME ?? "/nonexistent", ".Xauthority");
    const runtime = join(scratch, "runtime");
    mkdirSync(runtime, { mode: 0o700 });
    log = openSync(join(output, "electron.log"), "wx");
    child = spawn(electron, [
      app, "--kiosk", "--hostd", hostd, "--ui", ui,
      "--node", existsSync(node) ? node : process.execPath,
    ], {
      detached: true,
      stdio: ["ignore", log, log],
      env: {
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        LANG: process.env.LANG ?? "C.UTF-8",
        DISPLAY: process.env.DISPLAY,
        ...(existsSync(xauth) ? { XAUTHORITY: xauth } : {}),
        HOME: scratch,
        XDG_CONFIG_HOME: join(scratch, "config"),
        XDG_DATA_HOME: join(scratch, "xdg-data"),
        XDG_CACHE_HOME: join(scratch, "cache"),
        XDG_RUNTIME_DIR: runtime,
        OPENSHELL_SMOKE_TEST: output,
        OPENSHELL_SMOKE_URL: url,
        OPENSHELL_HOSTD_PORT: "0",
        OPENSHELL_HOSTD_ARGS: `--home ${join(scratch, "home")} --data-dir ${join(scratch, "hostd-data")} --offline`,
      },
    });
    if (child.pid) {
      poll = setInterval(() => {
        try {
          const sample = sandboxSample(child.pid);
          if (sample.length >= report.sandbox.length) report.sandbox = sample;
          if (sample.length >= 2 && sample.every((renderer) => renderer.filtered)) {
            report.sandbox = sample;
            clearInterval(poll);
          }
        } catch (error) {
          report.sandboxError = error.message;
          clearInterval(poll);
        }
      }, 750);
    }
    timeout = setTimeout(() => { timedOut = true; stop(); }, 180_000);
    process.once("SIGINT", stop);
    process.once("SIGTERM", stop);
    const [exitCode, signal] = await once(child, "close");
    assert.ok(!timedOut, "Electron timed out");
    assert.equal(exitCode, 0, `Electron exited with status ${exitCode ?? signal}`);
    const result = JSON.parse(readFileSync(join(output, "electron-smoke.json"), "utf8"));
    assert.ok(result.ok && result.preload && result.host === "desktop", "Desktop or hostd initialization failed");
    assert.ok(result.guests?.some((guest) => guest.url === url && guest.title === guestTitle), "Browser guest or worker failed");
    assert.ok(report.sandbox.length >= 2 && report.sandbox.every((renderer) => renderer.filtered),
      "Kernel did not provide evidence of renderer seccomp filters");
    report.ok = true;
  } catch (error) {
    report.error = error.message;
    throw error;
  } finally {
    clearInterval(poll);
    clearTimeout(timeout);
    process.removeListener("SIGINT", stop);
    process.removeListener("SIGTERM", stop);
    stop();
    if (log !== undefined) closeSync(log);
    await new Promise((done) => {
      server.close(done);
      server.closeAllConnections();
    });
    writeFileSync(join(output, "native-desktop.json"), `${JSON.stringify(report, null, 2)}\n`);
    rmSync(scratch, { recursive: true, force: true });
  }
}

const [bundle, electron, output, extra] = process.argv.slice(2);
if ((bundle === "--help" || bundle === "-h") && !electron) {
  console.log(usage);
} else if (!bundle || !electron || extra !== undefined) {
  console.error(usage);
  process.exitCode = 2;
} else {
  try {
    await smoke(bundle, electron, output);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
