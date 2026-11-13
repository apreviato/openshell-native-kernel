// Drives the real OpenShell Desktop in the Native shell: the Rust launcher
// starts hostd and the CEF host, and the test uses only X11 input and what
// hostd writes to disk. It covers Desktop startup, Files, Terminal, renderer
// and hostd recovery, the shutdown and a second session on the same profile.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { closeSync, existsSync, mkdirSync, mkdtempSync, openSync, readdirSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer as createHttpServer } from "node:http";
import { createServer } from "node:net";
import { arch, release, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { crc32, deflateSync, inflateSync } from "node:zlib";
import { sandboxSample } from "./linux-sandbox.mjs";
import { childOf, processes, resourceUse } from "./processes.mjs";

const usage = "Usage: node tools/desktop/shell-smoke.mjs <openshell-shell> <desktop-bundle> <cef-host> [output-dir]";
// Emulated guests run the Desktop much slower than a host.
const scale = Math.max(1, Number(process.env.OPENSHELL_CEF_SMOKE_TIMEOUT_MS ?? 45_000) / 45_000);
const WINDOW_TITLE = "^OpenShell Native$";
// The default Xvfb keymap maps F12 to keycode 96. Asking xdotool for
// the keysym F12 on that map also injects an unwanted Alt press.
const X11_F12_KEYCODE = "96";
// Print has the same kind of map (Alt gives Sys_Req): its keycode, 107, too.
const X11_PRINT_KEYCODE = "107";
// F2 and F3 too (their other levels switch virtual terminals).
const X11_F2_KEYCODE = "68";
const X11_F3_KEYCODE = "69";
// The key that types y on a US keyboard and z on a German one.
const X11_Y_KEYCODE = "29";

// The color of a pixel of a PNG that a canvas wrote (8-bit RGB or RGBA,
// not interlaced), as 0xRRGGBB.
function pngPixel(path, x, y) {
  const data = readFileSync(path);
  let offset = 8;
  let width = 0;
  let height = 0;
  let channels = 0;
  const chunks = [];
  while (offset < data.length) {
    const length = data.readUInt32BE(offset);
    const type = data.toString("latin1", offset + 4, offset + 8);
    const body = data.subarray(offset + 8, offset + 8 + length);
    if (type === "IHDR") {
      width = body.readUInt32BE(0);
      height = body.readUInt32BE(4);
      assert.equal(body[8], 8, "The PNG is not 8-bit");
      channels = { 2: 3, 6: 4 }[body[9]];
      assert.ok(channels && body[12] === 0, "The PNG is not plain RGB(A)");
    } else if (type === "IDAT") {
      chunks.push(body);
    }
    offset += 12 + length;
  }
  assert.ok(x < width && y < height, "The point is outside the PNG");
  const raw = inflateSync(Buffer.concat(chunks));
  const stride = width * channels;
  let previous = Buffer.alloc(stride);
  for (let row = 0; row <= y; row++) {
    const filter = raw[row * (stride + 1)];
    const line = Buffer.from(raw.subarray(row * (stride + 1) + 1, (row + 1) * (stride + 1)));
    for (let index = 0; index < stride; index++) {
      const left = index >= channels ? line[index - channels] : 0;
      const up = previous[index];
      const corner = index >= channels ? previous[index - channels] : 0;
      const base = left + up - corner;
      const nearest = Math.abs(base - left) <= Math.abs(base - up) && Math.abs(base - left) <= Math.abs(base - corner)
        ? left : Math.abs(base - up) <= Math.abs(base - corner) ? up : corner;
      const add = [0, left, up, (left + up) >> 1, nearest][filter];
      line[index] = (line[index] + add) & 0xff;
    }
    previous = line;
  }
  const pixel = previous.subarray(x * channels, x * channels + 3);
  return (pixel[0] << 16) | (pixel[1] << 8) | pixel[2];
}

function tool(command, args, { optional = false } = {}) {
  const result = spawnSync(command, args, { encoding: "utf8", timeout: 10_000 });
  if (result.status !== 0 || result.error) {
    if (optional) return null;
    throw new Error(`${command} ${args[0]} failed: ${result.error?.message ?? result.stderr?.trim() ?? result.status}`);
  }
  return result.stdout.trim();
}

function alive(pid) {
  try {
    // A zombie has exited; only its parent's wait is missing.
    return !/^State:\s+Z/m.test(readFileSync(`/proc/${pid}/status`, "utf8"));
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
  const evidence = { page: false, worker: false, webgl: null, clicks: 0, addressNavigation: false, addressReady: false, key: false, popup: false, widths: [], heights: [], devtools: false, chooser: null, pasted: null, generated: null, fullscreen: [], downloaded: false, reachedHostd: false, blocked: { fetch: false, socket: false } };
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
const reportSize = () => post("size?width=" + innerWidth + "&height=" + innerHeight);
addEventListener("resize", reportSize);
reportSize();
// z downloads a file.
addEventListener("keydown", (event) => { if (event.isTrusted && event.key === "z") location.href = "download"; });
// u chooses a file to upload: the Desktop's picker answers for the page.
const upload = Object.assign(document.createElement("input"), { type: "file", hidden: true });
document.body.append(upload);
upload.addEventListener("cancel", () => post("chooser?result=cancel"));
upload.addEventListener("change", () => post("chooser?result=" + encodeURIComponent(upload.files[0]?.name ?? "")));
addEventListener("keydown", (event) => { if (event.isTrusted && event.key === "u") upload.click(); });
// x selects a text for Ctrl+C; once copied, the other field waits for the paste.
const copied = Object.assign(document.createElement("input"), { value: "openshell-clipboard-probe" });
const pasted = document.createElement("input");
document.body.append(copied, pasted);
addEventListener("keydown", (event) => {
  if (event.isTrusted && event.key === "x" && document.activeElement !== copied && document.activeElement !== pasted) {
    event.preventDefault();
    copied.focus();
    copied.select();
  }
});
copied.addEventListener("copy", () => setTimeout(() => pasted.focus(), 100));
pasted.addEventListener("input", () => post("pasted?value=" + encodeURIComponent(pasted.value)));
// n gives the keyboard to a new password field: the password manager offers a strong password.
const signup = document.createElement("form");
const account = Object.assign(document.createElement("input"), { name: "username", autocomplete: "username", value: "openshell" });
const secret = Object.assign(document.createElement("input"), { type: "password", name: "password", autocomplete: "new-password" });
signup.append(account, secret);
document.body.append(signup);
addEventListener("keydown", (event) => {
  if (event.isTrusted && event.key === "n" && document.activeElement !== secret) {
    event.preventDefault();
    secret.focus();
  }
});
secret.addEventListener("input", () => post("generated?length=" + secret.value.length));
// f puts the page in fullscreen, as a video player does.
addEventListener("keydown", (event) => {
  if (event.isTrusted && event.key === "f" && document.activeElement !== secret) document.documentElement.requestFullscreen().catch(() => post("fullscreen?state=refused"));
});
document.addEventListener("fullscreenchange", () => post("fullscreen?state=" + (document.fullscreenElement ? "on" : "off")));
let opened = false;
window.addEventListener("keydown", (event) => {
  if (!event.isTrusted || event.key !== "k" || opened) return;
  opened = true;
  post("key");
  window.open("popup", "_blank");
});
if (location.pathname === "/address-focus") post("addressReady");`;
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
        else if (name === "size") {
          evidence.widths.push(Number(url.searchParams.get("width")));
          evidence.heights.push(Number(url.searchParams.get("height")));
        }
        else if (name === "reached") evidence.reachedHostd = true;
        else if (name === "chooser") evidence.chooser = url.searchParams.get("result");
        else if (name === "pasted") evidence.pasted = url.searchParams.get("value");
        else if (name === "generated") evidence.generated = Number(url.searchParams.get("length"));
        else if (name === "fullscreen") evidence.fullscreen.push(url.searchParams.get("state"));
        else if (name === "webgl") evidence.webgl = body;
        else if (name === "click") evidence.clicks++;
        else if (["page", "worker", "key", "devtools", "addressReady"].includes(name)) evidence[name] = true;
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

  /** The screen at half its size, as a PNG: printed when a run fails where its artifacts stay behind (the guest). */
  function screenPng(path, step = 2) {
    const data = readFileSync(path);
    const field = index => data.readUInt32BE(index * 4);
    if (field(2) !== 2 || field(11) !== 32) return null;
    const width = Math.floor(field(4) / step);
    const height = Math.floor(field(5) / step);
    const rows = Buffer.alloc((width * 3 + 1) * height);
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const pixel = data.readUInt32LE(field(0) + field(19) * 12 + y * step * field(12) + x * step * 4);
        const at = y * (width * 3 + 1) + 1 + x * 3;
        rows[at] = (pixel >> 16) & 255;
        rows[at + 1] = (pixel >> 8) & 255;
        rows[at + 2] = pixel & 255;
      }
    }
    const chunk = (type, body) => {
      const head = Buffer.alloc(4);
      head.writeUInt32BE(body.length);
      const tail = Buffer.alloc(4);
      tail.writeUInt32BE(crc32(Buffer.concat([Buffer.from(type), body])));
      return Buffer.concat([head, Buffer.from(type), body, tail]);
    };
    const header = Buffer.alloc(13);
    header.writeUInt32BE(width, 0);
    header.writeUInt32BE(height, 4);
    header.set([8, 2, 0, 0, 0], 8);
    return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", header), chunk("IDAT", deflateSync(rows)), chunk("IEND", Buffer.alloc(0))]);
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
      report.windowsAtFailure = visibleWindows();
      throw error;
    }
  }

  /** The visible X windows, with their place: what covers the screen when a check fails. */
  function visibleWindows() {
    const ids = (tool("xdotool", ["search", "--onlyvisible", "--name", ".*"], { optional: true }) ?? "").split("\n").filter(Boolean);
    return ids.slice(0, 40).map((id) => {
      const pid = Number(tool("xdotool", ["getwindowpid", id], { optional: true }) ?? 0);
      const geometry = (tool("xdotool", ["getwindowgeometry", id], { optional: true }) ?? "").replace(/\s+/g, " ").trim();
      // A picture of each window, by itself: which one covers the screen.
      tool("xwd", ["-id", id, "-silent", "-out", join(output, `failure-window-${id}.xwd`)], { optional: true });
      return {
        id,
        name: tool("xdotool", ["getwindowname", id], { optional: true }) ?? "",
        geometry,
        pid: pid || null,
        command: pid ? (processes().get(pid)?.cmdline ?? []).slice(0, 3).join(" ") : null,
      };
    });
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
        await waitFor("the page opened from the Desktop address bar to accept input", () => evidence.addressNavigation && evidence.addressReady, 30_000);
        // The renderer may have installed its listeners before the native view
        // is visible again. Wait for the fixture's green background at the click.
        let nextCapture = 0;
        await waitFor("the navigated page to be visible", () => {
          const now = performance.now();
          if (now < nextCapture) return false;
          nextCapture = now + 1000;
          const pixel = screenPixel(screenshot("browser-address-loaded"), pageX, pageY);
          return pixel === null || (pixel & 0xffffff) === 0x228844;
        }, 30_000);
        clickPage();
        await waitFor("a click after address navigation", () => evidence.clicks === 2, 30_000);
        // The Desktop switcher stays above the tab until the held Alt key is released.
        const pagePixel = screenPixel(screenshot("browser-before-switcher"), pageX, pageY);
        tool("xdotool", ["keydown", "Alt_L", "key", "Tab"]);
        try {
          if (pagePixel === null) {
            await pause(1);
          } else {
            await waitFor("the switcher to uncover the Desktop while Alt is held", () => {
              const pixel = screenPixel(screenshot("browser-switcher"), pageX, pageY);
              return pixel !== null && pixel !== pagePixel;
            }, 15_000);
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
        // Drive the real DevTools frontend. A viewport resize alone would also
        // pass if the Desktop reserved space for a frontend that never drew.
        const height = evidence.heights.at(-1);
        tool("xdotool", ["key", X11_F12_KEYCODE]);
        await waitFor("F12 to open docked DevTools", () => evidence.heights.at(-1) < height, 30_000);
        tool("xdotool", ["mousemove", String(pageX), String(Math.floor(display[1] / 4)), "click", "1"]);
        // The frontend may still be loading in a slow guest: the console
        // shortcut and the typing are tried again until the page hears them.
        for (let attempt = 1; !evidence.devtools; attempt++) {
          tool("xdotool", ["key", "ctrl+shift+j"]);
          await pause(2);
          screenshot(`browser-devtools-${attempt}`);
          await type(`fetch('/devtools', {method:'POST'})`);
          tool("xdotool", ["key", "Return"]);
          const reached = await waitFor("console input to reach the inspected page", () => evidence.devtools, 30_000).catch((error) => {
            if (attempt >= 3) throw error;
            return false;
          });
          if (reached) break;
        }
        tool("xdotool", ["key", X11_F12_KEYCODE]);
        await waitFor("F12 to close DevTools from its frontend", () => evidence.heights.at(-1) === height, 30_000);
        // The notification's left corner overlaps the tab, clear of the
        // Downloads sidebar that opens on the right.
        const toastX = display[0] - 14 - Math.min(420, display[0] - 28);
        const toastY = display[1] - 89;
        const beforeToast = await waitFor("the tab behind the notification position", () => {
          const shot = screenshot("browser-before-notification");
          const pixel = screenPixel(shot, toastX, toastY);
          return pixel === null ? { canCheck: false } : (pixel & 0xffffff) === 0x228844 && { canCheck: true };
        }, 10_000);
        // A download goes to Home > Downloads.
        tool("xdotool", ["key", "z"]);
        const download = join(home, "Downloads", "openshell-probe.txt");
        await waitFor("the download", () => existsSync(download) && readFileSync(download, "utf8") === "OpenShell Native download\n", 30_000);
        if (beforeToast.canCheck) {
          const shown = await waitFor("the download notification above the tab", () => {
            const shot = screenshot("browser-notification");
            const pixel = screenPixel(shot, toastX + 40, toastY - 18);
            return pixel !== null && (pixel & 0xffffff) !== 0x228844 && shot;
          }, 10_000);
          assert.equal(screenPixel(shown, pageX, pageY) & 0xffffff, 0x228844, "The notification hid the tab");
          const corner = screenPixel(shown, toastX, toastY);
          // The CSS shadow can darken the corner. The green page must still show through.
          const green = ((corner >> 8) & 255) - Math.max((corner >> 16) & 255, corner & 255);
          assert.ok(green >= 50, "The notification's rounded corner covered the tab");
        }
        // An upload opens the Desktop's file picker, not a dialog of its own.
        const windowsBefore = tool("xdotool", ["search", "--onlyvisible", "--name", ".+"], { optional: true }).split("\n").length;
        tool("xdotool", ["key", "u"]);
        await pause(3);
        screenshot("browser-file-chooser");
        const windowsAfter = tool("xdotool", ["search", "--onlyvisible", "--name", ".+"], { optional: true }).split("\n").length;
        assert.equal(windowsAfter, windowsBefore, "The page opened a file dialog outside the Desktop");
        tool("xdotool", ["key", "Escape"]);
        await waitFor("the page to learn that the choice was cancelled", () => evidence.chooser === "cancel", 30_000);
        // Ctrl+P prints the page: the tab as a PDF, in the Desktop's print dialog.
        clickPage();
        const printed = Date.now();
        tool("xdotool", ["key", "ctrl+p"]);
        await waitFor("the page as a PDF for printing", () => {
          const folder = join(tmpdir(), "openshell-print");
          return existsSync(folder) && readdirSync(folder).some((name) => name.endsWith(".pdf") && statSync(join(folder, name)).mtimeMs >= printed - 1000);
        }, 60_000);
        await pause(2);
        screenshot("browser-print");
        tool("xdotool", ["key", "Escape"]);
        await pause(1);
        // Shift+Print saves the whole screen, the Browser tab's own view included.
        tool("xdotool", ["mousemove", "4", String(display[1] - 4), "click", "1"]);
        await pause(1);
        tool("xdotool", ["key", `shift+${X11_PRINT_KEYCODE}`]);
        const pictures = join(home, "Pictures", "Screenshots");
        const picture = await waitFor("the screenshot in Pictures", () => {
          const name = existsSync(pictures) && readdirSync(pictures).find((entry) => entry.endsWith(".png"));
          return name ? join(pictures, name) : null;
        }, 60_000);
        await pause(1);
        assert.equal(pngPixel(picture, pageX, pageY), 0x228844, "The screenshot lacks the Browser tab");
        clickPage();
        // A copy in the page reaches the clipboard history; Super+V pastes it back.
        tool("xdotool", ["key", "x"]);
        await pause(1);
        tool("xdotool", ["key", "ctrl+c"]);
        await pause(2);
        tool("xdotool", ["key", "super+v"]);
        await pause(2);
        screenshot("browser-clipboard-history");
        tool("xdotool", ["key", "Return"]);
        await waitFor("the clipboard history to paste into the page", () => evidence.pasted === "openshell-clipboard-probe", 30_000);
        clickPage();
        // The brightness key from the page: the display shows above the tab, which
        // stays visible and keeps the keyboard. The display sits above the dock.
        const osdX = Math.floor(display[0] / 2);
        const osdY = display[1] - 100;
        const underOsd = screenPixel(screenshot("browser-before-osd"), osdX, osdY);
        tool("xdotool", ["key", "XF86MonBrightnessUp"]);
        if (underOsd !== null && (underOsd & 0xffffff) === 0x228844) {
          // The OSD expires after 1.6 seconds, even when smoke delays are scaled.
          const shown = await waitFor("the brightness display above the tab", () => {
            const shot = screenshot("browser-osd");
            const pixel = screenPixel(shot, osdX, osdY);
            return pixel !== null && (pixel & 0xffffff) !== 0x228844 && shot;
          }, 10_000);
          assert.equal(screenPixel(shown, pageX, pageY) & 0xffffff, 0x228844, "The tab was hidden under the volume and brightness display");
        }
        await pause(3);
        // Saved passwords: a new password field gets a strong one from the list above the tab,
        // picked with the keys pressed in the field.
        clickPage();
        tool("xdotool", ["key", "n"]);
        await pause(2);
        screenshot("browser-password-list");
        tool("xdotool", ["key", "Down"]);
        // The list tells the page's script that an entry is selected before Enter picks it.
        await pause(2);
        tool("xdotool", ["key", "Return"]);
        await waitFor("the suggested password in the page", () => evidence.generated >= 16, 30_000);
        // A page in fullscreen covers the whole screen, the Desktop included; Escape ends it.
        clickPage();
        tool("xdotool", ["key", "f"]);
        await waitFor("the page in fullscreen", () => evidence.fullscreen.includes("on"), 30_000);
        await waitFor("the fullscreen page over the whole screen", () => {
          const pixel = screenPixel(screenshot("browser-fullscreen"), 2, 2);
          return pixel === null || (pixel & 0xffffff) === 0x228844;
        }, 30_000);
        tool("xdotool", ["key", "Escape"]);
        await waitFor("Escape to end the fullscreen", () => evidence.fullscreen.at(-1) === "off", 30_000);
        await waitFor("the page back in its window", () => {
          const pixel = screenPixel(screenshot("browser-fullscreen-left"), 2, 2);
          return pixel === null || (pixel & 0xffffff) !== 0x228844;
        }, 30_000);
        clickPage();
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

    // The keyboard in the Terminal: Alt+S opens the search and types nothing
    // there, and the layout the X server has (hostd's keyboard service sets
    // it with setxkbmap) is the one the pages get.
    await check("keyboard", async () => {
      const typed = join(output, "keyboard.txt");
      await terminal("keyboard-terminal", `setxkbmap -layout de && cat > ${typed}`);
      try {
        tool("xdotool", ["key", "alt+s"]);
        await pause(2);
        tool("xdotool", ["key", "Escape"]);
        await pause(2);
        // Back in the Terminal, the newest window, in the middle of the screen.
        const [width, height] = tool("xdotool", ["getdisplaygeometry"]).split(" ").map(Number);
        tool("xdotool", ["mousemove", String(Math.floor(width / 2)), String(Math.floor(height / 2)), "click", "1"]);
        await pause(1);
        tool("xdotool", ["key", X11_Y_KEYCODE]);
        tool("xdotool", ["key", "Return"]);
        tool("xdotool", ["key", "ctrl+d"]);
        await waitFor("the line typed with the German layout", () => existsSync(typed) && readFileSync(typed, "utf8") === "z\n", 30_000)
          .catch((error) => { throw new Error(`${error.message}: ${JSON.stringify(existsSync(typed) ? readFileSync(typed, "utf8") : null)}`); });
      } finally {
        await terminal("keyboard-restore", "setxkbmap -layout us");
      }
    });

    // Super+Shift+R records an area of the screen into Videos, as on Live.
    await check("recording", async () => {
      const folder = join(home, "Videos", "Screen recordings");
      await keys("Escape", "super+shift+r");
      await pause(2);
      tool("xdotool", ["mousemove", "200", "150", "mousedown", "1", "mousemove", "520", "390", "mouseup", "1"]);
      await pause(1);
      tool("xdotool", ["key", "Return"]);
      await pause(5);
      screenshot("recording");
      tool("xdotool", ["key", "super+shift+r"]);
      const file = await waitFor("the screen recording in Videos", () => {
        const name = existsSync(folder) && readdirSync(folder).find((entry) => entry.endsWith(".webm"));
        return name && statSync(join(folder, name)).size > 10_000 ? join(folder, name) : null;
      }, 60_000);
      report.recording = { bytes: statSync(file).size };
    });

    // With the Linux apps profile (OPENSHELL_SMOKE_LINUX_APPS=1): a GTK and a
    // Qt application open from the Desktop's search in Desktop windows, through
    // Xpra and its HTML5 client, take the keyboard and the window's size, show
    // the GTK one's dialog and share its copy with the Desktop's clipboard
    // history, and close with their window or by themselves, leaving no process behind.
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
          { app: "gtk", title: "OpenShell GTK Probe", close: "alt+F4", dialogs: true },
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
          // Xpra maps the window when the client connects; the Desktop shows it once its first frame is drawn.
          await pause(2);
          screenshot(`linux-${probe.app}`);
          // The new window is in the middle of the screen, above the others.
          tool("xdotool", ["mousemove", String(Math.floor(width / 2)), String(Math.floor(height / 2)), "click", "1"]);
          await pause(1);
          await type(`native ${probe.app}`);
          tool("xdotool", ["key", "Return"]);
          await waitFor(`text typed into the ${probe.app} window`, () => events(probe.app).some((event) => event.event === "text" && event.value === `native ${probe.app}`), 60_000);
          if (probe.dialogs) {
            // A dialog of the application shows in the Desktop window and takes the keyboard.
            tool("xdotool", ["key", X11_F2_KEYCODE]);
            await waitFor(`the ${probe.app} dialog in the Desktop`, () => shownBy(start, `${probe.title} Dialog`), 60_000);
            await pause(1);
            screenshot(`linux-${probe.app}-dialog`);
            await type("native dialog");
            tool("xdotool", ["key", "Return"]);
            await waitFor(`text typed into the ${probe.app} dialog`, () => events(probe.app).some((event) => event.event === "dialog-text" && event.value === "native dialog"), 60_000);
            await pause(1);
            // A copy in the application reaches the Desktop's clipboard history,
            // and Super+V pastes it back where the keyboard was: into the application.
            tool("xdotool", ["key", X11_F3_KEYCODE]);
            const copied = await waitFor(`the ${probe.app} copy`, () => events(probe.app).find((event) => event.event === "copied"), 30_000);
            await pause(2);
            tool("xdotool", ["key", "ctrl+a"]);
            tool("xdotool", ["key", "super+v"]);
            await pause(2);
            screenshot(`linux-${probe.app}-clipboard-history`);
            tool("xdotool", ["key", "Return"]);
            await pause(2);
            tool("xdotool", ["key", "Return"]);
            await waitFor(`the ${probe.app} copy pasted back from the clipboard history`, () => events(probe.app).some((event) => event.event === "text" && event.value === copied.value), 60_000);
          }
          // The Desktop's shortcut maximizes the window, and the application takes its size.
          const before = sizes(probe.app).at(-1);
          tool("xdotool", ["key", "super+Up"]);
          await waitFor(`the ${probe.app} application to take the maximized size`, () => {
            const last = sizes(probe.app).at(-1);
            return last && before && last.width > before.width && last.height > before.height;
          }, 60_000);
          screenshot(`linux-${probe.app}-maximized`);
          // Its window is the active one again (the clipboard history had the keyboard), as a click makes it.
          tool("xdotool", ["mousemove", String(Math.floor(width / 2)), String(Math.floor(height / 2)), "click", "1"]);
          await pause(1);
          tool("xdotool", ["key", probe.close]);
          await waitFor(`the ${probe.app} application and its Xpra to exit`, () => !alive(start.pid) && xpraServers().length === 0, 60_000);
          report.linuxApps[probe.app] = { toolkit: start.toolkit, shownMs: shown, size: sizes(probe.app).at(-1), totalMs: Math.round(performance.now() - began) };
        }
        // A document of a type only the GTK application declares: open in the
        // Terminal and Enter in Files offer it, and it opens with the file.
        const folder = join(home, "openwith");
        mkdirSync(folder, { recursive: true });
        const documentPath = join(folder, "probe.odt");
        writeFileSync(documentPath, "OpenShell Native\n");
        report.linuxApps.openWith = {};
        for (const via of ["terminal", "files"]) {
          const before = new Set(events("gtk").map((event) => event.pid));
          if (via === "terminal") {
            await terminal("open-with-terminal", `open ${documentPath}`);
          } else {
            await terminal("open-with-files", `files ${folder}`);
            await pause(3);
            tool("xdotool", ["key", "ctrl+a"]);
            await pause(1);
            tool("xdotool", ["key", "Return"]);
          }
          // The Desktop asks which application opens the file, the GTK one
          // recommended: past the "Always" switch and Cancel, Open opens it.
          await pause(3);
          screenshot(`linux-open-with-${via}`);
          for (const key of ["Tab", "Tab", "Tab", "Return"]) {
            tool("xdotool", ["key", key]);
            await pause(0.5);
          }
          const opened = await waitFor(`the GTK application opened from ${via === "terminal" ? "the Terminal" : "Files"}`, () =>
            events("gtk").find((event) => event.event === "start" && !before.has(event.pid)), 120_000);
          assert.ok(opened.files?.some((file) => file.endsWith("openwith/probe.odt")), `The GTK application did not get the document: ${JSON.stringify(opened.files)}`);
          await waitFor(`the GTK window opened from ${via}`, () => shownBy(opened, "OpenShell GTK Probe"), 120_000);
          // Its window opens in the middle of the screen: a click makes it the active one.
          await pause(2);
          tool("xdotool", ["mousemove", String(Math.floor(width / 2)), String(Math.floor(height / 2)), "click", "1"]);
          await pause(1);
          tool("xdotool", ["key", "alt+F4"]);
          await waitFor(`the GTK application opened from ${via} to exit`, () => !alive(opened.pid) && xpraServers().length === 0, 60_000);
          report.linuxApps.openWith[via] = "passed";
        }
        // With the Windows apps profile (OPENSHELL_SMOKE_WINE=1): a Windows
        // program (Wine's own Notepad) opens with Wine from the Terminal, in a
        // Desktop window through Xpra, and closing it ends Wine.
        if (process.env.OPENSHELL_SMOKE_WINE === "1") {
          const wine = realpathSync(tool("sh", ["-c", "command -v wine"]));
          const program = join(dirname(dirname(wine)), "lib", "wine", "x86_64-windows", "notepad.exe");
          assert.ok(existsSync(program), `Wine has no ${program}`);
          const exe = join(folder, "notepad.exe");
          writeFileSync(exe, readFileSync(program));
          const wineProcesses = () => [...processes()].filter(([, { cmdline }]) => cmdline.some((arg) => /wineserver|\.exe$|wine64?-preloader/i.test(arg)));
          await terminal("open-with-wine", `open ${exe}`);
          await pause(3);
          screenshot("linux-open-with-wine");
          for (const key of ["Tab", "Tab", "Tab", "Return"]) {
            tool("xdotool", ["key", key]);
            await pause(0.5);
          }
          // The program's window, on the X server Xpra runs for it (Wine sets its prefix up the first time).
          const notepad = await waitFor("Notepad in a Desktop window", () => {
            for (const [, { cmdline }] of processes()) {
              if (!cmdline[0]?.includes("Xvfb-for-Xpra")) continue;
              // Xpra starts it with -displayfd: each X socket is tried with its -auth.
              const auth = cmdline[cmdline.indexOf("-auth") + 1];
              for (const socket of existsSync("/tmp/.X11-unix") ? readdirSync("/tmp/.X11-unix") : []) {
                const display = /^X(\d+)$/.exec(socket)?.[1];
                if (display && auth && shownBy({ display: `:${display}`, xauthority: auth }, "Untitled - Notepad")) return { display: `:${display}`, auth };
              }
            }
            return null;
          }, 300_000);
          await pause(2);
          screenshot("linux-wine-notepad");
          // OpenShell's prefix, without drive Z: (the whole filesystem).
          const prefix = join(home, ".local", "share", "openshell", "wine", "prefixes", "default");
          assert.ok(existsSync(join(prefix, "system.reg")), "Wine did not use OpenShell's prefix");
          assert.ok(!readdirSync(join(prefix, "dosdevices")).includes("z:"), "The prefix maps drive Z: to the whole filesystem");
          tool("xdotool", ["mousemove", String(Math.floor(width / 2)), String(Math.floor(height / 2)), "click", "1"]);
          await pause(1);
          tool("xdotool", ["key", "alt+F4"]);
          await waitFor("Wine to end with its window", () => wineProcesses().length === 0 && xpraServers().length === 0, 120_000);
          report.linuxApps.wine = { display: notepad.display };
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
    try {
      const shot = screenshot("failure");
      const png = shot && screenPng(shot);
      if (png) console.log(`OPENSHELL_SMOKE_SCREEN ${png.toString("base64")}`);
    } catch {
      // No display to show.
    }
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
