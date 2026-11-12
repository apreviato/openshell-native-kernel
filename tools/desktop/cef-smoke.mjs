import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { closeSync, existsSync, mkdirSync, mkdtempSync, openSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { arch, release } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { sandboxSample } from "./linux-sandbox.mjs";

// Emulated guests run Chromium much slower than a host, so they can extend the deadlines.
const timeoutScale = Math.max(1, Number(process.env.OPENSHELL_CEF_SMOKE_TIMEOUT_MS ?? 45_000) / 45_000);
const usage = "Usage: node tools/desktop/cef-smoke.mjs <sample|host|isolation> <cef-executable> [output-dir]";

function tool(command, args, { optional = false } = {}) {
  const result = spawnSync(command, args, { encoding: "utf8", timeout: 5000 });
  if (result.status !== 0 || result.error) {
    if (optional) return null;
    throw new Error(`${command} failed: ${result.error?.message ?? result.stderr?.trim() ?? result.status}`);
  }
  return result.stdout.trim();
}

async function startFixture({ cookie, setCookie = false, forbiddenUrl, key = "k" } = {}) {
  const id = randomBytes(8).toString("hex");
  const entry = `/${id}/`;
  const title = `OpenShell CEF Probe ${id}`;
  const evidence = { ready: false, interactions: 0, viewport: null };
  if (cookie) evidence.cookieObserved = false;
  if (setCookie) evidence.crossOriginRequests = 0;
  if (forbiddenUrl) evidence.blocked = { direct: false, redirect: false, worker: false };
  const html = `<!doctype html><meta charset="utf-8"><title>${title}</title>
<link rel="stylesheet" href="style.css"><button type="button">CEF input probe</button>
<script src="probe.js"></script>`;
  const forbidden = forbiddenUrl
    ? `for (const [kind, url] of [["direct", ${JSON.stringify(forbiddenUrl)}], ["redirect", "redirect"]]) {
  void fetch(url, { mode: "no-cors", credentials: "include" })
    .catch(() => fetch("blocked?kind=" + kind, { method: "POST" }));
}
worker.postMessage(${JSON.stringify(forbiddenUrl)});`
    : "";
  const probe = `const button = document.querySelector("button");
let mouse = false;
let keyboard = false;
const worker = new Worker("worker.js");
let verifiedWorker = false;
function report() {
  if (!mouse || !keyboard || !verifiedWorker) return;
  mouse = keyboard = false;
  void fetch("result", { method: "POST" });
}
button.addEventListener("click", (event) => { mouse ||= event.isTrusted; report(); });
window.addEventListener("keydown", (event) => { keyboard ||= event.isTrusted && event.key === ${JSON.stringify(key)}; report(); });
worker.onmessage = ({ data }) => {
  if (data === "blocked-worker") void fetch("blocked?kind=worker", { method: "POST" });
  if (data === "openshell-cef") { verifiedWorker = true; report(); }
};
worker.postMessage("openshell-cef");
function reportSize() { void fetch("size?" + new URLSearchParams({ width: innerWidth, height: innerHeight }), { method: "POST" }); }
window.addEventListener("resize", reportSize);
reportSize();
void fetch("ready", { method: "POST" });
${forbidden}`;
  const worker = `self.onmessage = async ({ data }) => {
  if (data === "openshell-cef") return self.postMessage(data);
  try { await fetch(data, { mode: "no-cors", credentials: "include" }); }
  catch { self.postMessage("blocked-worker"); }
};`;
  const files = new Map([
    [entry, ["text/html; charset=utf-8", html]],
    [entry + "probe.js", ["text/javascript", probe]],
    [entry + "worker.js", ["text/javascript", worker]],
    [entry + "style.css", ["text/css", "button { position: fixed; left: 15%; top: 15%; width: 70%; height: 70%; }"]],
  ]);
  const connectPolicy = forbiddenUrl ? `'self' ${new URL(forbiddenUrl).origin}` : "'self'";
  const server = createServer((request, response) => {
    if (cookie && request.headers.cookie?.split(/;\s*/).includes(cookie)) {
      evidence.cookieObserved = true;
    }
    if (setCookie && request.url === entry + "private-resource") {
      evidence.crossOriginRequests++;
      response.writeHead(204).end();
      return;
    }
    if (forbiddenUrl && request.method === "GET" && request.url === entry + "redirect") {
      response.writeHead(302, { Location: forbiddenUrl }).end();
      return;
    }
    if (request.method === "POST" && request.headers.origin === `http://127.0.0.1:${server.address().port}`) {
      const target = new URL(request.url, "http://127.0.0.1");
      if (target.pathname === entry + "ready") evidence.ready = true;
      else if (target.pathname === entry + "result") evidence.interactions++;
      else if (target.pathname === entry + "size") {
        evidence.viewport = [Number(target.searchParams.get("width")), Number(target.searchParams.get("height"))];
      } else if (forbiddenUrl && target.pathname === entry + "blocked" && Object.hasOwn(evidence.blocked, target.searchParams.get("kind"))) {
        evidence.blocked[target.searchParams.get("kind")] = true;
      } else { response.writeHead(404).end(); return; }
      response.writeHead(204).end();
      return;
    }
    const asset = request.method === "GET" && files.get(request.url);
    if (!asset) { response.writeHead(404).end(); return; }
    response.writeHead(200, {
      "Content-Type": asset[0],
      "Content-Security-Policy": `default-src 'none'; script-src 'self'; worker-src 'self'; connect-src ${connectPolicy}; style-src 'self'`,
      "X-Content-Type-Options": "nosniff",
      ...(setCookie && request.url === entry ? { "Set-Cookie": `${cookie}; Path=/; HttpOnly; SameSite=Strict` } : {}),
    }).end(asset[1]);
  });
  try {
    const listening = once(server, "listening", { signal: AbortSignal.timeout(8000) });
    server.listen(0, "127.0.0.1");
    await listening;
    return { server, title, evidence, key, url: `http://127.0.0.1:${server.address().port}${entry}` };
  } catch (error) {
    server.close();
    throw error;
  }
}

async function smoke(mode, executable, outputArg) {
  assert.ok(process.platform === "linux" && process.getuid() !== 0 && process.env.DISPLAY,
    "Use an unprivileged Linux X11 session");
  assert.ok(existsSync(executable), "CEF executable is missing");
  tool("xdotool", ["--version"]);
  const root = resolve(outputArg ?? "out");
  mkdirSync(root, { recursive: true });
  const output = mkdtempSync(join(root, "cef-"));
  const home = mkdtempSync("/tmp/openshell-native-cef-");
  const profile = join(home, "profile");
  const runtime = join(home, "runtime");
  const report = { ok: false, mode, output, kernel: release(), arch: arch(), display: "X11", sandbox: [] };
  const fixtures = [];
  let child;
  let childError;
  let fd;
  const stop = () => {
    if (child?.pid && child.exitCode === null && child.signalCode === null) {
      try { process.kill(-child.pid, "SIGKILL"); }
      catch (error) { if (error.code !== "ESRCH") console.error(error.message); }
    }
  };

  async function waitFor(label, predicate, duration = 45_000) {
    const deadline = performance.now() + duration * timeoutScale;
    while (performance.now() < deadline) {
      if (childError) throw childError;
      const result = predicate();
      if (result) return result;
      if (child?.exitCode != null || child?.signalCode != null) {
        throw new Error(`CEF exited before ${label} (${child.exitCode ?? child.signalCode})`);
      }
      await sleep(250);
    }
    throw new Error(`Timed out waiting for ${label}`);
  }

  function windowFor(fixture) {
    const ids = tool("xdotool", ["search", "--all", "--onlyvisible", "--pid", String(child.pid), "--name", `^${fixture.title}$`], { optional: true });
    return ids?.split("\n").find((value) => /^\d+$/.test(value));
  }

  function geometry(window) {
    const values = tool("xdotool", ["getwindowgeometry", "--shell", window]);
    const width = Number(values.match(/^WIDTH=(\d+)$/m)?.[1]);
    const height = Number(values.match(/^HEIGHT=(\d+)$/m)?.[1]);
    assert.ok(width > 0 && height > 0, "CEF window geometry is unavailable");
    return { width, height };
  }

  async function verifyInput(window, fixtures, round) {
    const { width, height } = geometry(window);
    for (const [index, fixture] of fixtures.entries()) {
      const x = Math.floor(width * (index + 0.5) / fixtures.length);
      const y = Math.floor(height / 2);
      tool("xdotool", ["windowactivate", "--sync", window, "mousemove", "--window", window, String(x), String(y), "click", "1", "key", fixture.key]);
      await waitFor("mouse, keyboard, and Web Worker", () => fixture.evidence.interactions === round);
    }
  }

  console.log(`Artifacts: ${output}`);
  try {
    mkdirSync(profile, { mode: 0o700 });
    mkdirSync(runtime, { mode: 0o700 });
    const isolation = mode === "isolation";
    const cookie = isolation ? `openshell_cef_isolation=${randomBytes(16).toString("hex")}` : undefined;
    const primary = await startFixture({ cookie, setCookie: isolation });
    fixtures.push(primary);
    report.evidence = primary.evidence;
    let guest;
    if (isolation) {
      guest = await startFixture({ cookie, key: "g", forbiddenUrl: primary.url + "private-resource" });
      fixtures.push(guest);
      report.guest = guest.evidence;
    }
    const auth = process.env.XAUTHORITY ?? join(process.env.HOME ?? "/nonexistent", ".Xauthority");
    fd = openSync(join(output, "cef.log"), "wx");
    child = spawn(executable, mode === "sample" ? ["--use-alloy-style", "--ozone-platform=x11", `--url=${primary.url}`] : [], {
      detached: true,
      stdio: [mode === "sample" ? "ignore" : "pipe", fd, fd],
      env: {
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        LANG: process.env.LANG ?? "C.UTF-8",
        DISPLAY: process.env.DISPLAY,
        ...(existsSync(auth) ? { XAUTHORITY: auth } : {}),
        HOME: home,
        XDG_CONFIG_HOME: join(home, "config"),
        XDG_CACHE_HOME: join(home, "cache"),
        XDG_RUNTIME_DIR: runtime,
        // A system without /etc/fonts names its font configuration explicitly.
        ...(process.env.FONTCONFIG_FILE ? { FONTCONFIG_FILE: process.env.FONTCONFIG_FILE } : {}),
        // Verbose Chromium logs in cef.log, for diagnosis only.
        ...(process.env.OPENSHELL_CEF_DIAGNOSTICS ? { OPENSHELL_CEF_DIAGNOSTICS: "1" } : {}),
      },
    });
    child.once("error", (error) => { childError = error; });
    process.once("SIGINT", stop);
    process.once("SIGTERM", stop);
    if (mode !== "sample") {
      child.stdin.once("error", (error) => { childError = error; });
      child.stdin.end([isolation ? "isolation" : "probe", primary.url, profile, ...(guest ? [guest.url] : []), ""].join("\n"));
    }

    await waitFor("fixture startup", () => fixtures.every(({ evidence }) => evidence.ready && evidence.viewport?.[0] > 0));
    const window = await waitFor("the CEF X11 window", () => windowFor(primary));
    await verifyInput(window, fixtures, 1);
    if (guest) {
      assert.ok(primary.evidence.cookieObserved, "The trusted context did not store its fixture cookie");
      await waitFor("the guest's blocked cross-origin requests", () => Object.values(guest.evidence.blocked).every(Boolean));
      assert.equal(primary.evidence.crossOriginRequests, 0, "The guest reached the private fixture endpoint");
      assert.equal(guest.evidence.cookieObserved, false, "The guest received the trusted context cookie");

      const widths = fixtures.map(({ evidence }) => evidence.viewport[0]);
      const { width, height } = geometry(window);
      tool("xdotool", ["windowsize", "--sync", window, String(width > 760 ? width - 160 : width + 160), String(height)]);
      await waitFor("both viewports to resize", () => fixtures.every(({ evidence }, index) => Math.abs(evidence.viewport[0] - widths[index]) >= 32));
      await verifyInput(window, fixtures, 2);
    }
    report.screenshot = tool("xwd", ["-silent", "-id", window, "-out", join(output, "cef.xwd")], { optional: true }) !== null;
    await waitFor("renderer sandbox evidence", () => {
      report.sandbox = sandboxSample(child.pid);
      return report.sandbox.length >= fixtures.length && report.sandbox.every((renderer) => renderer.filtered);
    }, 8000);
    tool("xdotool", ["windowactivate", "--sync", window, "key", "alt+F4"]);
    await waitFor("graceful exit", () => child.exitCode === 0, 15000);
    report.ok = true;
  } catch (error) {
    report.error = error.message;
    throw error;
  } finally {
    process.removeListener("SIGINT", stop);
    process.removeListener("SIGTERM", stop);
    stop();
    if (fd !== undefined) closeSync(fd);
    await Promise.all(fixtures.map(({ server }) => new Promise((done) => {
      server.close(done);
      server.closeAllConnections();
    })));
    writeFileSync(join(output, "cef-smoke.json"), `${JSON.stringify(report, null, 2)}\n`);
    rmSync(home, { recursive: true, force: true });
  }
}

const [mode, executable, output, extra] = process.argv.slice(2);
if (["--help", "-h"].includes(mode) && !executable) {
  console.log(usage);
} else if (!["sample", "host", "isolation"].includes(mode) || !executable || extra !== undefined) {
  console.error(usage);
  process.exitCode = 2;
} else {
  try {
    await smoke(mode, resolve(executable), output);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
