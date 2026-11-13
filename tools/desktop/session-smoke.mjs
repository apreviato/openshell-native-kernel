// Runs the OpenShell Native session under its supervisor, as init would:
// root starts openshell-session, which starts X, the window manager and the
// shell as an unprivileged user. The test kills the CEF host and expects a
// clean restart of the shell, then stops the session and expects every
// process of it to end.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { chmodSync, chownSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { join, resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const usage = "Usage: node tools/desktop/session-smoke.mjs <openshell-session> <openshell-shell> <desktop-bundle> <cef-host> <Xvfb> <openbox> [output-dir]";
const scale = Math.max(1, Number(process.env.OPENSHELL_CEF_SMOKE_TIMEOUT_MS ?? 45_000) / 45_000);
const USER = 65534;
const DISPLAY = process.env.OPENSHELL_SESSION_SMOKE_DISPLAY ?? ":5";

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
      // Exited while the table was read.
    }
  }
  return table;
}

/** The processes under `root`, with their command lines. */
function descendants(root) {
  const table = processes();
  const found = new Map();
  const queue = [root];
  while (queue.length) {
    const parent = queue.shift();
    for (const [pid, info] of table) {
      if (info.parent === parent && !found.has(pid)) {
        found.set(pid, info.cmdline);
        queue.push(pid);
      }
    }
  }
  return found;
}

/** Every process, busiest first: what held the CPU when a check failed, inside the session or not. */
function busiest(supervisorPid) {
  const session = supervisorPid ? new Set(descendants(supervisorPid).keys()) : new Set();
  const ticks = [];
  for (const entry of readdirSync("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    try {
      const stat = readFileSync(`/proc/${entry}/stat`, "utf8");
      const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
      const cmdline = readFileSync(`/proc/${entry}/cmdline`, "utf8").split("\0").filter(Boolean).slice(0, 3).join(" ");
      ticks.push({ pid: Number(entry), state: fields[0], cpuTicks: Number(fields[11]) + Number(fields[12]), session: session.has(Number(entry)), cmdline });
    } catch {
      // Exited while the table was read.
    }
  }
  return ticks.sort((a, b) => b.cpuTicks - a.cpuTicks).slice(0, 25);
}

function find(root, predicate) {
  for (const [pid, cmdline] of descendants(root)) {
    if (predicate(cmdline)) return pid;
  }
  return null;
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

async function smoke([supervisorPath, launcher, bundle, host, xvfb, openbox], outputArg) {
  assert.ok(process.platform === "linux" && process.getuid() === 0, "The supervisor test runs as root");
  const root = resolve(outputArg ?? "out");
  mkdirSync(root, { recursive: true });
  const output = mkdtempSync(join(root, "session-"));
  const scratch = mkdtempSync("/tmp/openshell-native-session-");
  // The session user reaches its home and state through it.
  chmodSync(scratch, 0o755);
  const state = join(scratch, "state");
  const logs = join(scratch, "logs");
  mkdirSync(state, { mode: 0o700 });
  chownSync(state, USER, USER);
  const report = { ok: false, output, checks: {} };
  let supervisor;
  console.log(`Artifacts: ${output}`);

  async function waitFor(label, predicate, duration = 120_000) {
    const deadline = performance.now() + duration * scale;
    while (performance.now() < deadline) {
      const result = predicate();
      if (result) return result;
      if (supervisor.exitCode !== null || supervisor.signalCode !== null) {
        throw new Error(`The supervisor exited before ${label} (${supervisor.exitCode ?? supervisor.signalCode})`);
      }
      await sleep(250);
    }
    throw new Error(`Timed out waiting for ${label}`);
  }

  const window = () => {
    const result = spawnSync("xdotool", ["search", "--onlyvisible", "--name", "^OpenShell Native$"], {
      encoding: "utf8", env: { ...process.env, DISPLAY }, timeout: 10_000,
    });
    return result.status === 0 && result.stdout.trim().split("\n")[0];
  };

  async function check(name, body) {
    try {
      await body();
      report.checks[name] = "passed";
    } catch (error) {
      report.checks[name] = "failed";
      throw error;
    }
  }

  try {
    // Confirm only after the real renderer and hostd have reported health.
    const confirmed = join(scratch, "confirmed");
    const confirmScript = join(scratch, "confirm.sh");
    writeFileSync(confirmScript, `#!/bin/sh\necho confirmed >> ${confirmed}\n`, { mode: 0o755 });
    const config = [
      `user=${USER}:${USER}`,
      `display=${DISPLAY}`,
      `home=${join(scratch, "home")}`,
      `log_dir=${logs}`,
      `x=${xvfb} ${DISPLAY} -screen 0 1280x800x24 -nolisten tcp -ac`,
      `wm=${openbox}`,
      `shell=${launcher} ${bundle} ${host} ${state} ${process.execPath}`,
      `env=PATH=${process.env.PATH ?? "/usr/bin:/bin"}`,
      `confirm=/bin/sh ${confirmScript}`,
      "confirm_after=10",
      `env=OPENSHELL_NATIVE_PORT=${await freePort()}`,
      ...["FONTCONFIG_FILE", "XDG_CONFIG_DIRS"].filter((name) => process.env[name]).map((name) => `env=${name}=${process.env[name]}`),
    ].join("\n");
    const configPath = join(scratch, "session.conf");
    writeFileSync(configPath, `${config}\n`, { mode: 0o600 });
    supervisor = spawn(supervisorPath, [configPath], { stdio: ["ignore", "inherit", "inherit"] });

    let first;
    await check("session", async () => {
      await waitFor("the Desktop window", window);
      await waitFor("the Desktop UI", () => existsSync(join(state, "hostd", "docs", "Welcome.md")));
      first = {
        launcher: find(supervisor.pid, (cmdline) => cmdline[0] === launcher),
        cef: find(supervisor.pid, (cmdline) => cmdline[0] === host),
        hostd: find(supervisor.pid, (cmdline) => cmdline.some((arg) => arg.endsWith("openshell-hostd.mjs"))),
      };
      assert.ok(first.launcher && first.cef && first.hostd, `The session is incomplete: ${JSON.stringify(first)}`);
      const owner = readFileSync(`/proc/${first.cef}/status`, "utf8").match(/^Uid:\s+(\d+)/m)?.[1];
      assert.equal(Number(owner), USER, "The shell does not run as the session user");
      // The supervisor keeps only CAP_FOWNER, CAP_KILL, CAP_SETGID and CAP_SETUID.
      const capabilities = readFileSync(`/proc/${supervisor.pid}/status`, "utf8").match(/^CapEff:\s+([0-9a-f]+)/m)?.[1];
      assert.equal(BigInt(`0x${capabilities}`), 0xe8n, `The supervisor keeps capabilities ${capabilities}`);
    });

    await check("shell-restart", async () => {
      // A crash of the CEF host ends the shell; the supervisor starts it again.
      process.kill(first.cef, "SIGKILL");
      assert.ok(!existsSync(confirmed), "The initial session was confirmed before the stability window");
      const second = await waitFor("a new shell", () => {
        const launcherPid = find(supervisor.pid, (cmdline) => cmdline[0] === launcher);
        const cef = find(supervisor.pid, (cmdline) => cmdline[0] === host);
        return launcherPid && launcherPid !== first.launcher && cef && cef !== first.cef && { launcher: launcherPid, cef };
      });
      await waitFor("the Desktop window again", window);
      for (const [name, pid] of Object.entries(first)) {
        assert.ok(!alive(pid), `The previous ${name} (pid ${pid}) survived the restart`);
      }
      report.restarted = second;
    });

    await check("confirm", async () => {
      // The restarted renderer must complete a new stability window.
      await waitFor("the boot to be confirmed", () => existsSync(confirmed));
      await new Promise((done) => setTimeout(done, 7_000));
      assert.equal(readFileSync(confirmed, "utf8"), "confirmed\n", "The boot was confirmed more than once");
    });

    await check("shutdown", async () => {
      const members = [...descendants(supervisor.pid).keys()];
      supervisor.kill("SIGTERM");
      await new Promise((done) => {
        if (supervisor.exitCode !== null) done();
        else supervisor.once("exit", done);
      });
      assert.equal(supervisor.exitCode, 0, `The supervisor exited with ${supervisor.exitCode ?? supervisor.signalCode}`);
      const survivors = members.filter(alive);
      assert.deepEqual(survivors, [], "Processes of the session survived it");
      const journal = readFileSync(join(logs, "0001", "session.log"), "utf8");
      assert.match(journal, /restarting the shell/, "The journal does not record the restart");
      assert.match(journal, /confirmed the boot/, "The journal does not record the confirmation");
      for (const name of ["session.log", "x.log", "shell.log"]) {
        writeFileSync(join(output, name), readFileSync(join(logs, "0001", name)));
      }
    });
    report.ok = true;
  } catch (error) {
    report.error = error.message;
    // The scratch folder goes away: what the session wrote, and what ran, stay in the report.
    report.logsAtFailure = {};
    const runs = existsSync(logs) ? readdirSync(logs, { withFileTypes: true }).filter((entry) => entry.isDirectory()) : [];
    for (const run of runs) {
      for (const name of readdirSync(join(logs, run.name))) {
        const text = readFileSync(join(logs, run.name, name), "utf8");
        report.logsAtFailure[`${run.name}/${name}`] = text.split("\n").slice(-60).join("\n");
      }
    }
    report.processesAtFailure = busiest(supervisor?.pid);
    console.error(JSON.stringify({ logsAtFailure: report.logsAtFailure, processesAtFailure: report.processesAtFailure }, null, 2));
    throw error;
  } finally {
    if (supervisor && supervisor.exitCode === null && supervisor.signalCode === null) {
      supervisor.kill("SIGTERM");
      for (let tries = 0; tries < 60 && supervisor.exitCode === null && supervisor.signalCode === null; tries++) {
        await sleep(250);
      }
      if (supervisor.exitCode === null && supervisor.signalCode === null) supervisor.kill("SIGKILL");
    }
    writeFileSync(join(output, "session-smoke.json"), `${JSON.stringify(report, null, 2)}\n`);
    rmSync(scratch, { recursive: true, force: true });
  }
}

const args = process.argv.slice(2);
if (args.length === 1 && ["--help", "-h"].includes(args[0])) {
  console.log(usage);
} else if (args.length < 6 || args.length > 7) {
  console.error(usage);
  process.exitCode = 2;
} else {
  try {
    await smoke(args.slice(0, 6).map((path) => realpathSync(path)), args[6]);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
