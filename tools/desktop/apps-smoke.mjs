// The Linux apps smoke: hostd opens Linux applications through Xpra, as the
// Desktop does, serves Xpra's HTML5 client and connects it, and a session ends
// without leaving processes behind, whether the Desktop closes it or the
// application exits. Input and resizing need the client in a Desktop window:
// the Desktop smoke covers them (shell-smoke.mjs, linux-apps).
//
// Usage: node tools/desktop/apps-smoke.mjs <desktop-bundle> [output-dir]
//
// The PATH provides xpra, dbus-run-session and the probe applications
// (userspace/compatibility/probes), whose desktop entries are in
// XDG_DATA_DIRS. Each probe appends JSON lines to ~/openshell-app-probe.log.

import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { copyFile, mkdir, mkdtemp, open, readdir, realpath, writeFile } from "node:fs/promises";
import http from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";

const usage = "Usage: node tools/desktop/apps-smoke.mjs <desktop-bundle> [output-dir]";
const ready = /^OPENSHELL_HOSTD_READY (http:\/\/127\.0\.0\.1:[1-9]\d{0,4})\/\?token=([A-Za-z0-9_-]{32,128})$/;
// Emulated guests run everything many times slower than the control: the
// same budget as the Desktop smoke (shell-smoke.mjs).
const scale = Math.max(1, Number(process.env.OPENSHELL_CEF_SMOKE_TIMEOUT_MS ?? 45_000) / 45_000);
const PROBES = [
  { id: "probe-gtk.desktop", app: "gtk", close: "session" },
  { id: "probe-qt.desktop", app: "qt", close: "application" },
];

async function waitFor(label, check, ms = 60_000) {
  const deadline = Date.now() + ms * scale;
  let last;
  while (Date.now() < deadline) {
    try {
      const value = await check();
      if (value) return value;
    } catch (error) {
      if (error.fatal) throw error;
      last = error;
    }
    await sleep(250);
  }
  throw new Error(`Timed out waiting for ${label}${last ? ` (${last.message})` : ""}`);
}

function readyUrl(child) {
  const lines = createInterface({ input: child.stdout });
  return new Promise((resolveUrl, reject) => {
    const timer = setTimeout(() => reject(new Error("Timed out waiting for hostd")), 60_000 * scale);
    child.once("exit", (code) => reject(new Error(`hostd exited (${code})`)));
    lines.on("line", (line) => {
      const match = ready.exec(line);
      if (match) {
        clearTimeout(timer);
        resolveUrl({ origin: match[1], token: match[2] });
      }
    });
  });
}

/** The processes whose command line contains the text. */
function processesWith(text) {
  const found = [];
  for (const entry of readdirSync("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    try {
      const command = readFileSync(`/proc/${entry}/cmdline`, "utf8").replaceAll("\0", " ");
      if (command.includes(text)) found.push(Number(entry));
    } catch {
      // The process exited.
    }
  }
  return found;
}

function alive(pid) {
  try {
    // A zombie has exited; only its parent's wait is missing.
    return !/^State:\s+Z/m.test(readFileSync(`/proc/${pid}/status`, "utf8"));
  } catch {
    return false;
  }
}

/** The probe's reports, in order. */
function events(log, app) {
  if (!existsSync(log)) return [];
  return readFileSync(log, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line)).filter((event) => event.app === app);
}

/** Opens a WebSocket to the application's Xpra through hostd and returns the status line. */
function upgrade(origin, token, path) {
  return new Promise((resolveStatus, reject) => {
    const request = http.request(new URL(path, origin), {
      headers: {
        connection: "Upgrade",
        upgrade: "websocket",
        "sec-websocket-version": "13",
        "sec-websocket-key": Buffer.from("openshell-native").toString("base64"),
        "sec-websocket-protocol": "binary",
        origin,
        "x-openshell-token": token,
      },
      timeout: 30_000 * scale,
    });
    request.on("upgrade", (response, socket) => {
      socket.destroy();
      resolveStatus(response.statusCode);
    });
    request.on("response", (response) => {
      response.resume();
      resolveStatus(response.statusCode);
    });
    request.on("timeout", () => request.destroy(new Error("The WebSocket upgrade timed out")));
    request.on("error", reject);
    request.end();
  });
}

async function smoke(bundleArg, outputArg) {
  const outputRoot = resolve(outputArg ?? "out");
  await mkdir(outputRoot, { recursive: true });
  const output = await mkdtemp(join(outputRoot, "apps-"));
  const report = { ok: false, output, checks: {}, timings: {}, apps: {} };
  console.log(`Artifacts: ${output}`);
  let hostd;
  let scratch;
  // hostd removes a session's directory when it ends: keep Xpra's log first.
  const keepLogs = async () => {
    for (const entry of scratch ? await readdir(scratch).catch(() => []) : []) {
      if (entry.startsWith("openshell-app-")) {
        await copyFile(join(scratch, entry, "server.log"), join(output, `${entry}.log`)).catch(() => undefined);
      }
    }
  };
  try {
    const bundle = await realpath(bundleArg);
    scratch = await realpath(await mkdtemp(join(tmpdir(), "openshell-apps-")));
    const home = join(scratch, "home");
    await mkdir(home);
    const probeLog = join(home, "openshell-app-probe.log");
    const log = await open(join(output, "hostd.log"), "w");
    const env = { ...process.env };
    for (const name of ["OPENSHELL_HOSTD_TOKEN", "OPENSHELL_MODE", "NODE_OPTIONS", "DISPLAY", "WAYLAND_DISPLAY"]) delete env[name];
    hostd = spawn(process.execPath, [join(bundle, "hostd", "openshell-hostd.mjs"), "--offline", "--host", "127.0.0.1", "--port", "0",
      "--ui", join(bundle, "ui"), "--home", home, "--data-dir", join(scratch, "data")], {
      env: { ...env, HOME: home, XDG_CONFIG_HOME: join(scratch, "config"), XDG_DATA_HOME: join(scratch, "xdg-data"), TMPDIR: scratch },
      stdio: ["ignore", "pipe", log.fd],
    });
    const { origin, token } = await readyUrl(hostd);
    const api = async (path, options = {}) => {
      const response = await fetch(new URL(path, origin), {
        ...options,
        headers: { "x-openshell-token": token, "content-type": "application/json", ...options.headers },
        signal: AbortSignal.timeout(120_000 * scale),
      });
      const type = response.headers.get("content-type") ?? "";
      const body = type.includes("json") ? await response.json() : await response.text();
      return { status: response.status, type, body };
    };

    const catalog = await api("/_native/apps");
    assert.equal(catalog.status, 200, JSON.stringify(catalog.body));
    assert.equal(catalog.body.available, true, "hostd does not find Xpra");
    for (const probe of PROBES) {
      assert.ok(catalog.body.apps.some((app) => app.id === probe.id), `The catalog lacks ${probe.id}: ${catalog.body.apps.map((app) => app.id).join(", ")}`);
    }
    report.checks.catalog = "passed";

    for (const probe of PROBES) {
      const result = { checks: {} };
      report.apps[probe.app] = result;
      const started = Date.now();
      const xServersBefore = new Set(processesWith("Xvfb"));
      const launch = await api("/_native/apps/launch", { method: "POST", body: JSON.stringify({ id: probe.id }) });
      assert.equal(launch.status, 200, `Launch failed: ${JSON.stringify(launch.body)}`);
      const session = launch.body;
      result.timings = { launch: Date.now() - started };
      const path = new URL(session.url, origin).pathname;
      // The Xpra processes of this session: its socket directory is private to it.
      const servers = await waitFor("the Xpra server", () => {
        const found = processesWith("--start-child=").filter((pid) => !report.servers?.includes(pid));
        return found.length ? found : null;
      });
      report.servers = [...(report.servers ?? []), ...servers];

      // The Desktop's window loads the HTML5 client and connects to Xpra through hostd.
      const page = await api(path);
      assert.equal(page.status, 200, "The Xpra client page did not load");
      assert.match(page.type, /^text\/html/);
      assert.equal(await upgrade(origin, token, path), 101, "hostd did not connect the client to Xpra");
      // Xpra accepts the password that hostd gives the client, and only it.
      const port = readFileSync(`/proc/${servers[0]}/cmdline`, "utf8").split("\0")
        .map((arg) => /^--bind-tcp=127\.0\.0\.1:(\d+),/.exec(arg)?.[1]).find(Boolean);
      const passwords = { right: new URL(session.url, origin).searchParams.get("password"), wrong: "not-the-password" };
      for (const [name, password] of Object.entries(passwords)) {
        await writeFile(join(scratch, `${name}.password`), password, { mode: 0o600 });
        const info = spawnSync("xpra", ["info", `tcp://127.0.0.1:${port}/`, `--password-file=${join(scratch, `${name}.password`)}`],
          { encoding: "utf8", timeout: 60_000 * scale, env: { ...process.env, HOME: home } });
        assert.equal(info.status === 0, name === "right", `Xpra ${name === "right" ? "refused" : "accepted"} the ${name} password: ${info.stderr.slice(-400)}`);
      }
      result.checks.client = "passed";

      const start = await waitFor(`the ${probe.app} window`, async () => {
        const event = events(probeLog, probe.app).find((entry) => entry.event === "start");
        if (event) return event;
        // hostd keeps the end of Xpra's log when the application closes.
        const status = await api(`${path}_status`);
        if (status.body?.error) throw Object.assign(new Error(status.body.error), { fatal: true });
        return null;
      });
      result.toolkit = start.toolkit;
      const xServers = processesWith("Xvfb").filter((pid) => !xServersBefore.has(pid));
      assert.ok(xServers.length, "Xpra runs no X server of its own");
      result.timings.window = Date.now() - started;
      // Closing the window in the Desktop ends the session, and an
      // application that exits ends it too: no process stays behind.
      await keepLogs();
      if (probe.close === "session") {
        const closed = await api(`${path}`, { method: "DELETE" });
        assert.equal(closed.status, 200);
      } else {
        process.kill(start.pid, "SIGTERM");
        await waitFor("hostd to report the application closed", async () => {
          const status = await api(`${path}_status`);
          return status.status === 404 || status.body.exited !== undefined;
        });
      }
      await waitFor(`the ${probe.app} processes to exit`, () => !alive(start.pid) && !servers.some(alive) && !xServers.some(alive), 60_000);
      result.checks.close = "passed";
      result.timings.closed = Date.now() - started;
      report.checks[probe.app] = "passed";
    }
    report.ok = true;
  } catch (error) {
    report.error = error instanceof Error ? error.message : String(error);
  } finally {
    await keepLogs();
    if (hostd && hostd.exitCode === null) {
      hostd.kill("SIGTERM");
      await Promise.race([new Promise((done) => hostd.once("exit", done)), sleep(10_000)]);
    }
    await writeFile(join(output, "apps-smoke.json"), `${JSON.stringify(report, null, 2)}\n`);
    console.log(JSON.stringify(report, null, 2));
  }
  return report.ok;
}

if (!process.argv[2]) {
  console.error(usage);
  process.exit(2);
}
process.exit(await smoke(process.argv[2], process.argv[3]) ? 0 : 1);
