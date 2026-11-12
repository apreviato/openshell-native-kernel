import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdir, mkdtemp, open, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";

const usage = "Usage: node tools/desktop/hostd-smoke.mjs <desktop-bundle> [output-dir] [node]";
const ready = /^OPENSHELL_HOSTD_READY (http:\/\/127\.0\.0\.1:[1-9]\d{0,4}\/\?token=[A-Za-z0-9_-]{32,128})$/;
const timeout = 8000;

async function sha256(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

function launchUrl(child) {
  const lines = createInterface({ input: child.stdout });
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => finish(new Error("Timed out waiting for hostd readiness")), 30_000);
    const finish = (error, url) => {
      clearTimeout(timer);
      child.off("error", finish);
      lines.removeAllListeners("line");
      lines.removeAllListeners("close");
      if (error) reject(error);
      else resolve(url);
    };
    child.once("error", finish);
    lines.once("close", () => finish(new Error("hostd closed stdout before readiness")));
    lines.on("line", (line) => {
      if (!line.startsWith("OPENSHELL_HOSTD_READY ")) return;
      const match = ready.exec(line);
      if (match && URL.canParse(match[1])) finish(null, new URL(match[1]));
      else finish(new Error("Invalid hostd readiness URL"));
    });
  });
}

function probeSocket(url, headers, verify) {
  const socket = new WebSocket(url, { headers });
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => finish(new Error("Timed out waiting for hostd RPC")), timeout);
    const finish = (error, result) => {
      clearTimeout(timer);
      socket.onclose = null;
      socket.onerror = null;
      socket.onmessage = null;
      socket.close();
      if (error) reject(error);
      else resolve(result);
    };
    socket.onerror = () => finish(new Error("hostd WebSocket handshake failed"));
    socket.onclose = (event) => {
      if (!verify && event.code === 4401) finish(null);
      else finish(new Error(`Unexpected hostd WebSocket close (${event.code})`));
    };
    socket.onmessage = ({ data }) => {
      try {
        assert.ok(verify, "hostd accepted an unauthorized WebSocket");
        const result = verify(JSON.parse(data), socket);
        if (result) finish(null, result);
      } catch (error) {
        finish(error);
      }
    };
  });
}

// A small client for the hostd protocol. Results and events are plain JSON text frames
// for the calls below; none of them carries a binary payload.
class HostClient {
  static async connect(url, headers) {
    const client = new HostClient(new WebSocket(url, { headers }));
    await client.waitFor((message) => message.t === "hello", "hello");
    const hello = client.messages.find((message) => message.t === "hello");
    assert.ok(Number.isInteger(hello.apiVersion) && hello.apiVersion > 0, "Invalid host API version");
    client.apiVersion = hello.apiVersion;
    return client;
  }

  constructor(socket) {
    this.socket = socket;
    this.messages = [];
    this.nextId = 1;
    this.closed = null;
    socket.onmessage = ({ data }) => {
      if (typeof data === "string") this.messages.push(JSON.parse(data));
    };
    socket.onclose = (event) => { this.closed = event.code; };
  }

  async waitFor(predicate, what, deadline = timeout) {
    const end = Date.now() + deadline;
    while (Date.now() < end) {
      const found = this.messages.find(predicate);
      if (found) return found;
      if (this.closed !== null) throw new Error(`hostd closed the connection (${this.closed}) waiting for ${what}`);
      await sleep(25);
    }
    throw new Error(`Timed out waiting for ${what}`);
  }

  async call(method, params = {}, deadline = timeout) {
    const id = this.nextId++;
    this.socket.send(JSON.stringify({ t: "call", id, m: method, p: params }));
    const reply = await this.waitFor((message) => message.t === "res" && message.id === id, method, deadline);
    if (!reply.ok) throw new Error(`${method} failed: ${reply.e?.code} ${reply.e?.message}`);
    return reply.r;
  }

  events(topic, filter = () => true) {
    return this.messages.filter((message) => message.t === "evt" && message.topic === topic && filter(message.d));
  }

  close() {
    this.socket.close();
  }
}

// The workloads that Files, Memory, Terminal and the agent tools depend on.
const workloads = {
  // Files and Memory converge through the watcher after an atomic rename.
  async fileEvents(client, { home }) {
    const folder = join(home, "watched");
    await mkdir(folder);
    await writeFile(join(folder, "pending"), "openshell-native\n");
    const { watchId } = await client.call("fs.watch", { path: folder });
    await client.call("fs.rename", { from: join(folder, "pending"), to: join(folder, "ready") });
    await client.waitFor((message) => message.t === "evt" && message.topic === "fs.changed" && message.d.watchId === watchId,
      "fs.changed after a rename");
    const stat = await client.call("fs.stat", { path: join(folder, "ready") });
    assert.ok(stat && stat.size === "openshell-native\n".length, "fs.stat did not see the renamed file");
    await client.call("fs.unwatch", { watchId });
  },

  // The Terminal runs a real TTY that follows resizes, and its shell exit is reported.
  async terminal(client, { home }) {
    const opened = await client.call("pty.open", { cols: 100, rows: 30, shell: "/bin/sh", args: [], cwd: home });
    assert.notEqual(opened.backend, "pipe", `Linux hosts need a TTY backend, got ${opened.backend}`);
    const output = () => client.events("pty.data", (event) => event.id === opened.id).map((message) => message.d.data).join("");
    const expect = async (pattern, what) => {
      const end = Date.now() + timeout;
      while (!pattern.test(output()) && Date.now() < end) await sleep(25);
      assert.match(output(), pattern, `${what}: ${JSON.stringify(output().slice(-400))}`);
    };
    await client.call("pty.write", { id: opened.id, data: "tty; stty size; echo osh_marker_$((6 * 7))\r" });
    await expect(/\/dev\/pts\/\d+[\s\S]*30 100[\s\S]*osh_marker_42/, "PTY output");
    await client.call("pty.resize", { id: opened.id, cols: 132, rows: 43 });
    await client.call("pty.write", { id: opened.id, data: "stty size\r" });
    await expect(/43 132/, "PTY resize");
    await client.call("pty.write", { id: opened.id, data: "exit 3\r" });
    const exit = await client.waitFor((message) => message.t === "evt" && message.topic === "pty.exit" && message.d.id === opened.id,
      "pty.exit");
    assert.equal(exit.d.exitCode, 3, "The terminal reported another exit status");
  },

  // Agent tools run commands, read their output and stop them on a deadline.
  async commands(client, { home }) {
    const done = await client.call("exec.run", { command: "printf %s \"$PWD\"; echo err >&2; exit 5", cwd: home });
    assert.ok(done.stdout === home && done.stderr.trim() === "err" && done.exitCode === 5 && !done.timedOut,
      `exec.run returned ${JSON.stringify(done)}`);
    const started = Date.now();
    const stopped = await client.call("exec.run", { command: "sleep 30", timeoutMs: 500 }, 15_000);
    assert.ok(stopped.timedOut && Date.now() - started < 10_000, `exec.run ignored its deadline: ${JSON.stringify(stopped)}`);
  },
};

async function smoke(bundleArg, outputArg, node) {
  const outputRoot = resolve(outputArg ?? "out");
  await mkdir(outputRoot, { recursive: true });
  const output = await mkdtemp(join(outputRoot, "hostd-"));
  const report = { ok: false, node: process.version, platform: process.platform, checks: {} };
  let scratch;
  let log;
  let child;
  let closed;
  console.log(`Artifacts: ${output}`);
  try {
    const bundle = await realpath(bundleArg);
    const hostd = join(bundle, "hostd", "openshell-hostd.mjs");
    const ui = join(bundle, "ui");
    const build = JSON.parse(await readFile(join(bundle, "BUILD.json"), "utf8"));
    report.bundle = { version: build.version, builtAt: build.builtAt, hostdSha256: await sha256(hostd), uiSha256: await sha256(join(ui, "index.html")) };
    scratch = await realpath(await mkdtemp(join(tmpdir(), "openshell-hostd-")));
    const home = join(scratch, "home");
    const data = join(scratch, "data");
    await mkdir(home);
    await writeFile(join(home, "native-probe.txt"), "openshell-native\n");
    log = await open(join(output, "hostd.log"), "w");
    const env = { ...process.env };
    for (const name of ["OPENSHELL_HOSTD_TOKEN", "OPENSHELL_MODE", "OPENSHELL_ALLOW_POWER", "NODE_OPTIONS"]) delete env[name];
    child = spawn(node ?? process.execPath, [hostd, "--offline", "--host", "127.0.0.1", "--port", "0", "--ui", ui, "--home", home, "--data-dir", data], {
      env: { ...env, HOME: home, USERPROFILE: home, XDG_CONFIG_HOME: join(scratch, "config"), XDG_DATA_HOME: join(scratch, "xdg-data") },
      // hostd writes only errors to stderr. The session token goes to stdout.
      stdio: ["ignore", "pipe", log.fd],
      windowsHide: true,
    });
    closed = new Promise((resolve) => child.once("close", resolve));
    const url = await launchUrl(child);
    report.checks.ready = true;
    const request = (path, headers) => fetch(new URL(path, url), { headers, redirect: "manual", signal: AbortSignal.timeout(timeout) });
    const response = await request(url);
    await response.body?.cancel();
    assert.equal(response.status, 302);
    assert.ok(response.headers.get("location") === "/", "hostd did not remove the bootstrap query from the URL");
    const cookie = response.headers.get("set-cookie") ?? "";
    assert.ok(/^openshell_hostd=[A-Za-z0-9_-]+;/.test(cookie) && /; HttpOnly(?:;|$)/i.test(cookie) && /; SameSite=Strict(?:;|$)/i.test(cookie), "Missing protected hostd cookie");
    report.checks.bootstrap = true;
    const page = await request("/", { cookie: cookie.split(";", 1)[0] });
    assert.equal(page.status, 200);
    assert.match(page.headers.get("content-type") ?? "", /^text\/html/i);
    assert.equal(createHash("sha256").update(Buffer.from(await page.arrayBuffer())).digest("hex"), report.bundle.uiSha256);
    report.checks.ui = true;

    const ws = `ws://${url.host}/_host/ws`;
    const headers = { origin: url.origin, cookie: cookie.split(";", 1)[0] };
    await probeSocket(ws, { origin: url.origin });
    await probeSocket(ws, { ...headers, origin: "http://localhost" });
    report.checks.authorization = true;
    const client = await HostClient.connect(ws, headers);
    try {
      const info = await client.call("system.info");
      assert.ok(info.apiVersion === client.apiVersion, "system.info reported another API version");
      assert.ok(resolve(info.home) === home && resolve(info.dataDir) === data, "hostd used unexpected state directories");
      report.host = { apiVersion: client.apiVersion, version: info.hostdVersion, os: info.os };
      const listing = await client.call("fs.readdir", { path: home });
      assert.ok(listing.entries?.some((entry) => entry.name === "native-probe.txt"), "fs.readdir did not return the probe file");
      report.checks.rpc = true;

      for (const [name, run] of Object.entries(workloads)) {
        await run(client, { home });
        report.checks[name] = true;
      }
    } finally {
      client.close();
    }
    report.ok = true;
  } catch (error) {
    report.error = error.cause?.code ? `${error.message} (${error.cause.code})` : error.message;
    throw new Error(report.error);
  } finally {
    if (child?.pid && child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
      await Promise.race([closed, sleep(4000, undefined, { ref: false })]);
      if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    }
    await closed;
    await log?.close();
    await writeFile(join(output, "hostd-smoke.json"), `${JSON.stringify(report, null, 2)}\n`);
    if (scratch) await rm(scratch, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
  }
}

const [bundle, output, node, extra] = process.argv.slice(2);
if (["--help", "-h"].includes(bundle) && !output) {
  console.log(usage);
} else if (!bundle || extra !== undefined) {
  console.error(usage);
  process.exitCode = 2;
} else {
  try {
    await smoke(bundle, output, node);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
