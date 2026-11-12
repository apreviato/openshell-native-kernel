// Drives the real OpenShell Desktop in the Native shell: the Rust launcher
// starts hostd and the CEF host, and the test uses only X11 input and what
// hostd writes to disk. It covers Desktop startup, Files, Terminal, renderer
// and hostd recovery, the shutdown and a second session on the same profile.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { closeSync, existsSync, mkdirSync, mkdtempSync, openSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer as createHttpServer } from "node:http";
import { createServer } from "node:net";
import { arch, release } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { sandboxSample } from "./linux-sandbox.mjs";

const usage = "Usage: node tools/desktop/shell-smoke.mjs <openshell-shell> <desktop-bundle> <cef-host> [output-dir]";
// Emulated guests run the Desktop much slower than a host.
const scale = Math.max(1, Number(process.env.OPENSHELL_CEF_SMOKE_TIMEOUT_MS ?? 45_000) / 45_000);
const WINDOW_TITLE = "^OpenShell Native$";

function tool(command, args, { optional = false } = {}) {
  const result = spawnSync(command, args, { encoding: "utf8", timeout: 10_000 });
  if (result.status !== 0 || result.error) {
    if (optional) return null;
    throw new Error(`${command} ${args[0]} failed: ${result.error?.message ?? result.stderr?.trim() ?? result.status}`);
  }
  return result.stdout.trim();
}

function processes() {
  const table = new Map();
  for (const entry of readdirSync("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    try {
      const stat = readFileSync(`/proc/${entry}/stat`, "utf8");
      const parent = Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]);
      const cmdline = readFileSync(`/proc/${entry}/cmdline`, "utf8").split("\0").filter(Boolean);
      table.set(Number(entry), { parent, cmdline });
    } catch {
      // The process exited while the table was read.
    }
  }
  return table;
}

function childOf(parent, predicate) {
  for (const [pid, info] of processes()) {
    if (info.parent === parent && predicate(info.cmdline)) return pid;
  }
  return null;
}

/** Memory and processor time of `root` and its descendants. */
function resourceUse(root) {
  const table = processes();
  const members = new Set([root]);
  for (let grew = true; grew;) {
    grew = false;
    for (const [pid, info] of table) {
      if (!members.has(pid) && members.has(info.parent)) {
        members.add(pid);
        grew = true;
      }
    }
  }
  let rssKiB = 0;
  let pssKiB = 0;
  let pssAvailable = true;
  let cpuTicks = 0;
  for (const pid of members) {
    try {
      rssKiB += Number(readFileSync(`/proc/${pid}/status`, "utf8").match(/^VmRSS:\s+(\d+)/m)?.[1] ?? 0);
      // PSS divides shared pages, such as libcef.so, among the processes.
      try {
        const pss = readFileSync(`/proc/${pid}/smaps_rollup`, "utf8").match(/^Pss:\s+(\d+)/m)?.[1];
        if (pss === undefined) pssAvailable = false;
        else pssKiB += Number(pss);
      } catch {
        pssAvailable = false;
      }
      const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
      const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
      cpuTicks += Number(fields[11]) + Number(fields[12]);
    } catch {
      // Exited meanwhile.
    }
  }
  return {
    processes: members.size,
    // The sum of RSS counts shared pages once per process.
    rssMiB: Math.round(rssKiB / 1024),
    pssMiB: pssAvailable ? Math.round(pssKiB / 1024) : null,
    cpuSeconds: Math.round(cpuTicks) / 100,
  };
}

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function freePort() {
  const server = createServer();
  await new Promise((done, fail) => server.once("error", fail).listen(0, "127.0.0.1", done));
  const { port } = server.address();
  await new Promise((done) => server.close(done));
  return port;
}

// A site for the Desktop's Browser: it reports what the page saw and did,
// and tries to reach hostd, which the shell must prevent.
async function startSite(hostdPort) {
  const evidence = { page: false, worker: false, webgl: null, clicks: 0, addressNavigation: false, key: false, popup: false, widths: [], downloaded: false, reachedHostd: false, blocked: { fetch: false, socket: false } };
  const page = `<!doctype html><meta charset="utf-8"><title>OpenShell Browser Probe</title>
<style>html, body { margin: 0; height: 100%; background: #284; color: #fff; font: 24px sans-serif; }</style>
<body><h1>OpenShell Browser Probe</h1><script src="probe.js"></script></body>`;
  const probe = `const post = (path) => fetch(path, { method: "POST" }).catch(() => undefined);
post("page");
const worker = new Worker("worker.js");
worker.onmessage = ({ data }) => { if (data === "pong") post("worker"); };
worker.postMessage("ping");
const hostd = "http://127.0.0.1:${hostdPort}";
fetch(hostd + "/api/system", { mode: "no-cors", credentials: "include" }).then(() => post("reached"), () => post("blocked?kind=fetch"));
// CEF does not filter WebSocket handshakes: hostd refuses a foreign origin
// without its cookie and closes the socket before its first message.
const socket = new WebSocket("ws://127.0.0.1:${hostdPort}/_host/ws");
let message = false;
socket.onmessage = () => { message = true; post("reached"); };
socket.onclose = () => { if (!message) post("blocked?kind=socket"); };
let webgl = "unavailable";
try {
  const gl = document.createElement("canvas").getContext("webgl");
  const info = gl && gl.getExtension("WEBGL_debug_renderer_info");
  if (gl) webgl = String(gl.getParameter(info ? info.UNMASKED_RENDERER_WEBGL : gl.RENDERER));
} catch {}
fetch("webgl", { method: "POST", body: webgl.slice(0, 200) }).catch(() => undefined);
document.addEventListener("click", (event) => { if (event.isTrusted) post("click"); });
// The page's CSS width shrinks when the Browser zooms in.
const reportWidth = () => post("width?value=" + innerWidth);
addEventListener("resize", reportWidth);
reportWidth();
// z downloads a file.
addEventListener("keydown", (event) => { if (event.isTrusted && event.key === "z") location.href = "download"; });
let opened = false;
window.addEventListener("keydown", (event) => {
  if (!event.isTrusted || event.key !== "k" || opened) return;
  opened = true;
  post("key");
  window.open("popup", "_blank");
});`;
  const worker = `self.onmessage = ({ data }) => { if (data === "ping") self.postMessage("pong"); };`;
  const files = new Map([
    ["/", ["text/html; charset=utf-8", page]],
    ["/address-focus", ["text/html; charset=utf-8", page]],
    ["/probe.js", ["text/javascript", probe]],
    ["/worker.js", ["text/javascript", worker]],
    ["/popup", ["text/html; charset=utf-8", "<!doctype html><title>OpenShell Browser Popup</title><p>Popup</p>"]],
  ]);
  const server = createHttpServer((request, response) => {
    const url = new URL(request.url, "http://127.0.0.1");
    if (request.method === "POST") {
      let body = "";
      request.setEncoding("utf8");
      request.on("data", (chunk) => { body = (body + chunk).slice(0, 400); });
      request.on("end", () => {
        const name = url.pathname.slice(1);
        if (name === "blocked" && Object.hasOwn(evidence.blocked, url.searchParams.get("kind"))) evidence.blocked[url.searchParams.get("kind")] = true;
        else if (name === "width") evidence.widths.push(Number(url.searchParams.get("value")));
        else if (name === "reached") evidence.reachedHostd = true;
        else if (name === "webgl") evidence.webgl = body;
        else if (name === "click") evidence.clicks++;
        else if (["page", "worker", "key"].includes(name)) evidence[name] = true;
        response.writeHead(204).end();
      });
      return;
    }
    if (url.pathname === "/popup") evidence.popup = true;
    if (url.pathname === "/address-focus") evidence.addressNavigation = true;
    if (url.pathname === "/download") {
      evidence.downloaded = true;
      response.writeHead(200, { "Content-Type": "text/plain", "Content-Disposition": 'attachment; filename="openshell-probe.txt"' }).end("OpenShell Native download\n");
      return;
    }
    const file = request.method === "GET" && files.get(url.pathname);
    if (!file) {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, { "Content-Type": file[0], "Cache-Control": "no-store" }).end(file[1]);
  });
  await new Promise((done, fail) => server.once("error", fail).listen(0, "127.0.0.1", done));
  return { server, evidence, url: `http://127.0.0.1:${server.address().port}/` };
}

async function smoke(launcher, bundle, host, outputArg) {
  assert.ok(process.platform === "linux" && process.getuid() !== 0 && process.env.DISPLAY,
    "Use an unprivileged Linux X11 session");
  for (const [path, label] of [[launcher, "launcher"], [host, "CEF host"], [join(bundle, "BUILD.json"), "Desktop bundle"]]) {
    assert.ok(existsSync(path), `The ${label} is missing: ${path}`);
  }
  tool("xdotool", ["--version"]);
  const root = resolve(outputArg ?? "out");
  mkdirSync(root, { recursive: true });
  const output = mkdtempSync(join(root, "shell-"));
  const state = mkdtempSync("/tmp/openshell-native-shell-");
  const home = join(state, "home");
  const probe = join(home, "zz-files-probe");
  const build = JSON.parse(readFileSync(join(bundle, "BUILD.json"), "utf8"));
  const report = {
    ok: false, output, kernel: release(), arch: arch(), display: "X11",
    bundle: { version: build.version, builtAt: build.builtAt }, checks: {}, timings: {}, resources: {},
  };
  const port = await freePort();
  let session = null;
  console.log(`Artifacts: ${output}`);

  function screenshot(name) {
    const path = join(output, `${name}.xwd`);
    return tool("xwd", ["-root", "-silent", "-out", path], { optional: true }) === null ? null : path;
  }

  function screenPixel(path, x, y) {
    if (!path) return null;
    const data = readFileSync(path);
    const field = index => data.readUInt32BE(index * 4);
    if (field(2) !== 2 || field(11) !== 32 || x >= field(4) || y >= field(5)) return null;
    const offset = field(0) + field(19) * 12 + y * field(12) + x * 4;
    return data.readUInt32LE(offset);
  }

  async function waitFor(label, predicate, duration = 60_000) {
    const deadline = performance.now() + duration * scale;
    while (performance.now() < deadline) {
      const result = predicate();
      if (result) return result;
      if (session?.child.exitCode != null || session?.child.signalCode != null) {
        throw new Error(`The shell exited before ${label} (${session.child.exitCode ?? session.child.signalCode})`);
      }
      await sleep(250);
    }
    throw new Error(`Timed out waiting for ${label}`);
  }

  async function pause(seconds) {
    await sleep(seconds * 1000 * scale);
  }

  async function keys(...sequence) {
    tool("xdotool", ["windowactivate", "--sync", session.window]);
    for (const key of sequence) {
      tool("xdotool", ["key", "--clearmodifiers", key]);
      await pause(1.5);
    }
  }

  async function type(text) {
    tool("xdotool", ["type", "--delay", "40", text]);
    await pause(1);
  }

  async function start(label) {
    const began = performance.now();
    const log = openSync(join(output, `${label}.log`), "wx");
    const child = spawn(launcher, [bundle, host, state, process.execPath], {
      detached: true,
      stdio: ["ignore", log, log],
      env: {
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        LANG: process.env.LANG ?? "C.UTF-8",
        DISPLAY: process.env.DISPLAY,
        ...(process.env.XAUTHORITY ? { XAUTHORITY: process.env.XAUTHORITY } : {}),
        ...(process.env.FONTCONFIG_FILE ? { FONTCONFIG_FILE: process.env.FONTCONFIG_FILE } : {}),
        // Where the Linux applications' desktop entries are, when not in /usr/share.
        ...(process.env.XDG_DATA_DIRS ? { XDG_DATA_DIRS: process.env.XDG_DATA_DIRS } : {}),
        OPENSHELL_NATIVE_PORT: String(port),
      },
    });
    session = { child, log, label, window: null, cef: null };
    session.cef = await waitFor("the CEF host", () => childOf(child.pid, (cmdline) => cmdline[0] === host));
    session.window = await waitFor("the Desktop window", () =>
      tool("xdotool", ["search", "--onlyvisible", "--name", WINDOW_TITLE], { optional: true })?.split("\n")[0]);
    report.timings[`${label}Window`] = Math.round(performance.now() - began);
    return began;
  }

  async function stop() {
    const { child, log } = session;
    if (child.exitCode === null && child.signalCode === null) {
      process.kill(child.pid, "SIGTERM");
      await waitFor("the shell to exit", () => child.exitCode !== null || child.signalCode !== null, 30_000)
        .catch(() => undefined);
    }
    closeSync(log);
    const status = child.exitCode;
    if (status === null) {
      try { process.kill(-child.pid, "SIGKILL"); } catch { /* Already gone. */ }
    }
    session = null;
    return status;
  }

  // The resources of the shell's processes: CEF with its subprocesses, and hostd.
  function sample(label) {
    const hostd = hostdPid();
    report.resources[label] = { cef: resourceUse(session.cef), ...(hostd ? { hostd: resourceUse(hostd) } : {}) };
  }

  function hostdPid() {
    return childOf(session.child.pid, (cmdline) => cmdline.some((arg) => arg.endsWith("openshell-hostd.mjs")));
  }

  // Opens Terminal from the search field and runs a command that writes a
  // file, then the optional command, which runs only after the file exists.
  async function terminal(name, then = "") {
    const marker = join(output, `${name}.txt`);
    for (let attempt = 1; attempt <= 3; attempt++) {
      await keys("Escape", "alt+s");
      await type("terminal");
      await keys("Return");
      await pause(4);
      await type(`echo $((6*7)) > ${marker}${then ? ` && ${then}` : ""}`);
      await keys("Return");
      const done = await waitFor(`the ${name} command`, () => existsSync(marker) && readFileSync(marker, "utf8").trim() === "42", 30_000)
        .catch(() => false);
      if (done) {
        screenshot(name);
        return;
      }
      screenshot(`${name}-attempt-${attempt}`);
    }
    throw new Error(`Terminal did not run the ${name} command`);
  }

  async function check(name, body) {
    const began = performance.now();
    try {
      await body();
      report.checks[name] = "passed";
      report.timings[name] = Math.round(performance.now() - began);
    } catch (error) {
      report.checks[name] = "failed";
      screenshot(`${name}-failure`);
      throw error;
    }
  }

  try {
    mkdirSync(probe, { recursive: true, mode: 0o700 });
    writeFileSync(join(probe, "before.txt"), "OpenShell Native\n");

    let began;
    await check("desktop", async () => {
      began = await start("session-1");
      // The UI writes its default documents through hostd once it is running.
      await waitFor("the Desktop UI", () => existsSync(join(state, "hostd", "docs", "Welcome.md")), 120_000);
      report.timings.desktopReady = Math.round(performance.now() - began);
      await pause(3);
      screenshot("desktop");
      sample("desktopReady");
    });

    await check("files", async () => {
      // Files opens Home; the probe folder is its last entry.
      await keys("Escape", "alt+w");
      await pause(3);
      await keys("ctrl+a", "Return");
      await pause(2);
      await keys("ctrl+a", "F2", "ctrl+a");
      await type("after.txt");
      await keys("Return");
      await waitFor("Files to rename the probe", () => existsSync(join(probe, "after.txt")) && !existsSync(join(probe, "before.txt")), 30_000);
      screenshot("files");
    });

    await check("terminal", () => terminal("terminal"));

    await check("browser", async () => {
      // xdg-open in the Terminal opens the address in the Desktop's Browser,
      // whose tab is a view of the shell over the Browser window.
      const site = await startSite(port);
      try {
        await terminal("terminal-browser", `xdg-open ${site.url}`);
        const { evidence } = site;
        await waitFor("the Browser tab", () => evidence.page && evidence.worker && evidence.webgl !== null);
        await waitFor("the page to be kept away from hostd", () => evidence.blocked.fetch && evidence.blocked.socket, 30_000);
        assert.equal(evidence.reachedHostd, false, "A Browser page reached hostd");
        await pause(2);
        screenshot("browser");
        // The Browser window opens in the middle of the screen, its page under the pointer.
        const display = tool("xdotool", ["getdisplaygeometry"]).split(" ").map(Number);
        const pageX = Math.floor(display[0] / 2);
        const pageY = Math.floor(display[1] / 2) + 40;
        const clickPage = () => tool("xdotool", ["mousemove", String(pageX), String(pageY), "click", "1"]);
        clickPage();
        await waitFor("a click in the page", () => evidence.clicks === 1, 30_000);
        // Send directly to the focused view. Activating the top-level X11
        // window here would move the keyboard away from the Browser tab.
        tool("xdotool", ["key", "ctrl+l"]);
        await pause(1.5);
        await type(`${site.url}address-focus`);
        screenshot("browser-address");
        tool("xdotool", ["key", "Return"]);
        await waitFor("navigation typed into the Desktop address bar from the page", () => evidence.addressNavigation, 30_000);
        await pause(2);
        clickPage();
        await waitFor("a click after address navigation", () => evidence.clicks === 2, 30_000);
        // The Desktop switcher stays above the tab until the held Alt key is released.
        const pagePixel = screenPixel(screenshot("browser-before-switcher"), pageX, pageY);
        tool("xdotool", ["keydown", "Alt_L", "key", "Tab"]);
        try {
          await pause(1);
          const switcherPixel = screenPixel(screenshot("browser-switcher"), pageX, pageY);
          if (pagePixel !== null && switcherPixel !== null) {
            assert.notEqual(switcherPixel, pagePixel, "The switcher did not uncover the Desktop while Alt was held");
          }
          clickPage();
          await pause(1);
          assert.equal(evidence.clicks, 2, "The page received a click through the Desktop switcher");
          tool("xdotool", ["key", "Escape"]);
        } finally {
          tool("xdotool", ["keyup", "Alt_L"]);
        }
        await pause(1);
        clickPage();
        await waitFor("page input after dismissing the switcher", () => evidence.clicks === 3, 30_000);
        // The Browser's shortcuts work with the keyboard in the page.
        const before = evidence.widths.at(-1);
        tool("xdotool", ["key", "ctrl+equal"]);
        await waitFor("the page to zoom in", () => evidence.widths.at(-1) < before, 30_000);
        tool("xdotool", ["key", "ctrl+0"]);
        await waitFor("the zoom to reset", () => evidence.widths.at(-1) === before, 30_000);
        // A download goes to Home > Downloads.
        tool("xdotool", ["key", "z"]);
        const download = join(home, "Downloads", "openshell-probe.txt");
        await waitFor("the download", () => existsSync(download) && readFileSync(download, "utf8") === "OpenShell Native download\n", 30_000);
        // The download's notification covers the page for a moment; the page keeps the keyboard.
        await pause(1);
        tool("xdotool", ["key", "k"]);
        await waitFor("a key in the page", () => evidence.key, 30_000);
        await waitFor("the page's popup in a new tab", () => evidence.popup, 30_000);
        report.sandbox = await waitFor("filtered renderers with the Browser", () => {
          const renderers = sandboxSample(session.cef);
          return renderers.length > 1 && renderers.every((renderer) => renderer.filtered) && renderers;
        }, 15_000);
        sample("withBrowser");
        // The Desktop's shortcuts reach it from the page: Alt+S opens the search.
        await terminal("terminal-from-tab");
      } finally {
        report.browser = site.evidence;
        site.server.closeAllConnections();
        site.server.close();
      }
    });

    // With the Linux apps profile (OPENSHELL_SMOKE_LINUX_APPS=1): a GTK and a
    // Qt application open from the Desktop's search in Desktop windows, through
    // Xpra and its HTML5 client, take the keyboard and the window's size, and
    // close with their window or by themselves, leaving no process behind.
    if (process.env.OPENSHELL_SMOKE_LINUX_APPS === "1") {
      await check("linux-apps", async () => {
        const log = join(home, "openshell-app-probe.log");
        const events = (app) => (existsSync(log) ? readFileSync(log, "utf8") : "").split("\n").filter(Boolean)
          .map((line) => JSON.parse(line)).filter((event) => event.app === app);
        const sizes = (app) => events(app).filter((event) => event.event === "size");
        // Xpra maps an application's window once a client shows it.
        const shownBy = (start, title) => spawnSync("xdotool", ["search", "--onlyvisible", "--name", `^${title}$`], {
          env: { ...process.env, DISPLAY: start.display, XAUTHORITY: start.xauthority }, encoding: "utf8",
        }).stdout.trim();
        const xpraServers = () => [...processes().values()].filter(({ cmdline }) => cmdline.some((arg) => arg.startsWith("--start-child=")));
        const [width, height] = tool("xdotool", ["getdisplaygeometry"]).split(" ").map(Number);
        report.linuxApps = {};
        for (const probe of [
          // Alt+F4 closes the Desktop window, which ends the application.
          { app: "gtk", title: "OpenShell GTK Probe", close: "alt+F4" },
          // Escape makes the application close itself, which closes its window.
          { app: "qt", title: "OpenShell Qt Probe", close: "Escape" },
        ]) {
          const began = performance.now();
          await keys("Escape", "alt+s");
          await type(probe.title);
          await keys("Return");
          const start = await waitFor(`the ${probe.app} application`, () => events(probe.app).find((event) => event.event === "start"), 120_000);
          await waitFor(`the ${probe.app} window in the Desktop`, () => shownBy(start, probe.title), 120_000);
          const shown = Math.round(performance.now() - began);
          screenshot(`linux-${probe.app}`);
          // The new window is in the middle of the screen, above the others.
          tool("xdotool", ["mousemove", String(Math.floor(width / 2)), String(Math.floor(height / 2)), "click", "1"]);
          await pause(1);
          await type(`native ${probe.app}`);
          tool("xdotool", ["key", "Return"]);
          await waitFor(`text typed into the ${probe.app} window`, () => events(probe.app).some((event) => event.event === "text" && event.value === `native ${probe.app}`), 60_000);
          // The Desktop's shortcut maximizes the window, and the application takes its size.
          const before = sizes(probe.app).at(-1);
          tool("xdotool", ["key", "super+Up"]);
          await waitFor(`the ${probe.app} application to take the maximized size`, () => {
            const last = sizes(probe.app).at(-1);
            return last && before && last.width > before.width && last.height > before.height;
          }, 60_000);
          screenshot(`linux-${probe.app}-maximized`);
          tool("xdotool", ["key", probe.close]);
          await waitFor(`the ${probe.app} application and its Xpra to exit`, () => !alive(start.pid) && xpraServers().length === 0, 60_000);
          report.linuxApps[probe.app] = { toolkit: start.toolkit, shownMs: shown, size: sizes(probe.app).at(-1), totalMs: Math.round(performance.now() - began) };
        }
      });
    }

    await check("sandbox", async () => {
      report.desktopSandbox = await waitFor("filtered Desktop renderers", () => {
        const renderers = sandboxSample(session.cef);
        return renderers.length > 0 && renderers.every((renderer) => renderer.filtered) && renderers;
      }, 15_000);
    });

    await check("renderer-restart", async () => {
      const before = new Set(sandboxSample(session.cef).map(({ pid }) => pid));
      for (const pid of before) process.kill(pid, "SIGKILL");
      await waitFor("a new filtered renderer", () => {
        const renderers = sandboxSample(session.cef);
        return renderers.some(({ pid }) => !before.has(pid)) && renderers.every((renderer) => renderer.filtered);
      });
      await terminal("terminal-after-renderer");
      assert.ok(alive(session.cef), "The CEF host did not survive its renderer");
    });

    await check("hostd-restart", async () => {
      const hostd = hostdPid();
      assert.ok(hostd, "hostd is not running");
      process.kill(hostd, "SIGKILL");
      await waitFor("a new hostd", () => {
        const pid = hostdPid();
        return pid !== null && pid !== hostd;
      });
      await terminal("terminal-after-hostd");
      assert.ok(alive(session.cef), "The CEF host did not survive the hostd restart");
    });

    // At rest: no input for a while.
    await pause(5);
    sample("idle");

    await check("shutdown", async () => {
      const { cef } = session;
      const hostd = hostdPid();
      const status = await stop();
      assert.equal(status, 0, `The shell exited with ${status}`);
      await waitFor("the session processes to exit", () => !alive(cef) && !(hostd && alive(hostd)), 15_000);
    });

    await check("second-session", async () => {
      // Same port and profile: the Desktop origin and its storage persist.
      assert.ok(existsSync(join(state, "cef", "desktop")), "The Desktop profile was not kept on disk");
      await start("session-2");
      await terminal("terminal-second-session");
      assert.ok(existsSync(join(probe, "after.txt")), "The renamed file did not persist");
      const status = await stop();
      assert.equal(status, 0, `The second shell exited with ${status}`);
    });

    await check("secrets", async () => {
      // The bootstrap secret travels only through pipes.
      const logs = readdirSync(output).filter((name) => name.endsWith(".log")).map((name) => join(output, name));
      logs.push(join(state, "hostd.log"));
      for (const path of logs) {
        assert.ok(!readFileSync(path, "utf8").includes("token="), `${path} contains a bootstrap URL`);
      }
      const log = readFileSync(join(output, "session-1.log"), "utf8");
      assert.match(log, /desktop renderer terminated/, "The shell did not report the renderer exit");
      assert.match(log, /hostd exited with .*; restarting/, "The shell did not report the hostd restart");
    });
    report.ok = true;
  } catch (error) {
    report.error = error.message;
    throw error;
  } finally {
    if (session) await stop().catch(() => undefined);
    try {
      writeFileSync(join(output, "hostd.log"), readFileSync(join(state, "hostd.log")));
    } catch {
      // No session started.
    }
    writeFileSync(join(output, "shell-smoke.json"), `${JSON.stringify(report, null, 2)}\n`);
    rmSync(state, { recursive: true, force: true });
  }
}

const args = process.argv.slice(2);
if (["--help", "-h"].includes(args[0]) && args.length === 1) {
  console.log(usage);
} else if (args.length < 3 || args.length > 4) {
  console.error(usage);
  process.exitCode = 2;
} else {
  try {
    await smoke(realpathSync(args[0]), realpathSync(args[1]), realpathSync(args[2]), args[3]);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
