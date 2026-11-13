#!/usr/bin/env node
// Wine in userspace, without the Desktop (OPENSHELL_WINE.md, phase W1): in an
// isolated prefix, wineboot sets the prefix up, Notepad opens under X11 and
// closes with its window, and nothing of the prefix stays behind (wineserver
// included). Runs the same in the Linux of control and in the Asterinas guest.
//
//   node tools/desktop/wine-smoke.mjs <output-folder>
//
// Needs DISPLAY, and wine, wineboot, wineserver and xdotool on the PATH.
// OPENSHELL_CEF_SMOKE_TIMEOUT_MS scales the waits for slow guests.

import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const scale = Math.max(1, Number(process.env.OPENSHELL_CEF_SMOKE_TIMEOUT_MS ?? 45_000) / 45_000);

function run(command, args, options = {}) {
  return spawnSync(command, args, { encoding: "utf8", ...options });
}

async function waitFor(label, check, ms) {
  const deadline = Date.now() + ms * scale;
  while (Date.now() < deadline) {
    const value = await check();
    if (value) return value;
    await sleep(250);
  }
  throw new Error(`Timed out waiting for ${label}`);
}

/** The processes running with this prefix, as their environment says. */
function prefixProcesses(prefix) {
  const found = [];
  for (const entry of readdirSync("/proc")) {
    if (!/^\d+$/.test(entry)) continue;
    try {
      if (/^State:\s+Z/m.test(readFileSync(`/proc/${entry}/status`, "utf8"))) continue;
      const environ = readFileSync(`/proc/${entry}/environ`, "utf8").split("\0");
      if (environ.includes(`WINEPREFIX=${prefix}`)) {
        found.push({ pid: Number(entry), command: readFileSync(`/proc/${entry}/cmdline`, "utf8").split("\0").filter(Boolean).join(" ") });
      }
    } catch {
      // Gone, or not ours.
    }
  }
  return found;
}

/**
 * Alt+F4 to a window, made the active one first. Not `key --window`: when the
 * window goes away between the presses and the releases, xdotool stops there
 * and Alt and F4 stay held in the X server, closing every window after it.
 */
function closeWindow(window, env) {
  run("xdotool", ["windowactivate", "--sync", window], { env });
  run("xdotool", ["key", "alt+F4"], { env });
  run("xdotool", ["keyup", "alt", "F4"], { env });
}

/** A desktop entry's string and Exec quoting, both undone: Wine writes C:\\\\users as C:\users. */
function unescapeExec(value) {
  return value.replace(/\\(.)/g, "$1").replace(/\\(.)/g, "$1");
}

/** The .desktop files under a folder, with their text. */
function desktopEntries(folder) {
  if (!existsSync(folder)) return [];
  return readdirSync(folder, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".desktop"))
    .map((entry) => ({ name: entry.name, text: readFileSync(join(entry.parentPath, entry.name), "utf8") }));
}

/**
 * Gate W4 (OPENSHELL_WINE.md): an application installed by its installer into
 * a prefix of its own, opened from its shortcut, its settings kept, backed
 * up, removed and restored, carried to a newer Wine with a backup first, and
 * uninstalled. All through openshell-wine, as the Desktop does it, in the
 * person's data (XDG_DATA_HOME), which a system rollback does not touch.
 */
async function installedApplication(installer, output, report, baseEnv) {
  const data = join(output, "data");
  const env = { ...baseEnv, XDG_DATA_HOME: data, XDG_CACHE_HOME: join(output, "cache") };
  delete env.WINEPREFIX;
  const runner = (args) => run("openshell-wine", args, { env, timeout: 900_000 * scale });
  const ok = (result, what) => {
    assert.equal(result.status, 0, `${what} failed (${result.status ?? result.error?.message}): ${(result.stderr ?? "").slice(-600)}`);
    return result.stdout.trim().split("\n").at(-1);
  };
  const id = "openshell-probe-setup";
  const prefix = join(data, "openshell", "wine", "prefixes", id);
  const prefixEnv = { ...env, WINEPREFIX: prefix };
  const program = join(prefix, "drive_c", "Program Files", "OpenShell Probe", "probe.exe");
  const settings = () => readFileSync(join(prefix, "openshell-prefix.conf"), "utf8");
  const shortcuts = () => desktopEntries(join(data, "applications")).filter((entry) => entry.text.includes(`WINEPREFIX=${prefix}"`));
  const setting = () => {
    const result = runner(["--prefix", id, "reg", "query", "HKCU\\Software\\OpenShell\\Probe", "/v", "Setting"]);
    return /Setting\s+REG_SZ\s+(\S+)/.exec(ok(result, "Querying the setting"))?.[1] ?? null;
  };
  const settle = () => run("wineserver", ["-w"], { env: prefixEnv, timeout: 120_000 * scale });

  let began = Date.now();
  assert.equal(ok(runner(["--install", installer]), "The install"), id);
  assert.ok(existsSync(program), "The installer left no program");
  assert.match(settings(), /^install=done$/m);
  const [shortcut, ...more] = shortcuts();
  assert.ok(shortcut && more.length === 0, `Expected the installer's one shortcut: ${JSON.stringify(shortcuts().map((entry) => entry.name))}`);
  const associations = desktopEntries(join(data, "applications")).filter((entry) => entry.name.startsWith("wine-extension-"));
  assert.deepEqual(associations.map((entry) => entry.name), [], "Wine's file associations became Desktop entries");
  const users = join(prefix, "drive_c", "users");
  for (const user of readdirSync(users)) {
    const temp = join(users, user, "AppData", "Local", "Temp");
    if (existsSync(join(users, user, "AppData"))) assert.ok(lstatSync(temp).isSymbolicLink(), `${temp} is in the prefix, not the cache`);
  }
  report.timings.install = Date.now() - began;
  report.checks.install = "passed";

  // Opened from its shortcut as the Desktop runs it (hostd/native/wine.ts), and closed.
  began = Date.now();
  const target = unescapeExec(/ wine "([^"]+\.lnk)"/.exec(shortcut.text)?.[1] ?? "");
  assert.match(target, /^C:\\.*\.lnk$/, `The shortcut does not open a .lnk: ${shortcut.text}`);
  const opened = spawn("openshell-wine", ["--prefix", id, target], { env, stdio: "ignore" });
  const closed = new Promise((done) => opened.once("exit", done));
  const window = await waitFor("the installed program's window", () => {
    const found = run("xdotool", ["search", "--onlyvisible", "--name", "Notepad"], { env }).stdout.trim().split("\n").filter(Boolean);
    return found[0] ?? null;
  }, 120_000);
  closeWindow(window, env);
  await Promise.race([closed, sleep(60_000 * scale).then(() => { throw new Error("The installed program did not close"); })]);
  settle();
  report.timings.shortcut = Date.now() - began;
  report.checks.shortcut = "passed";

  // Its setting, then a change of it, outlive Wine (a new wineserver each time).
  assert.equal(setting(), "installed");
  ok(runner(["--prefix", id, "reg", "add", "HKCU\\Software\\OpenShell\\Probe", "/v", "Setting", "/d", "changed", "/f"]), "Changing the setting");
  settle();
  assert.equal(setting(), "changed");
  report.checks.settings = "passed";

  // Backed up, removed with its shortcut, restored with its setting.
  began = Date.now();
  const backup = ok(runner(["--backup", id]), "The backup");
  assert.ok(existsSync(backup), `No backup at ${backup}`);
  ok(runner(["--remove", id]), "The removal");
  assert.ok(!existsSync(prefix), "The prefix outlived its removal");
  assert.deepEqual(shortcuts(), [], "The shortcut outlived its prefix");
  assert.equal(ok(runner(["--restore", backup]), "The restore"), id);
  assert.ok(existsSync(program), "The restored prefix has no program");
  assert.equal(setting(), "changed");
  report.timings.backup = Date.now() - began;
  report.checks.backup = "passed";

  // Another Wine ran it last: backed up as it was, then updated, and recorded.
  began = Date.now();
  const wine = /^wine=(.*)$/m.exec(settings())?.[1];
  writeFileSync(join(prefix, "openshell-prefix.conf"), settings().replace(/^wine=.*$/m, "wine=wine-10.0"));
  assert.equal(setting(), "changed");
  assert.ok(existsSync(join(data, "openshell", "wine", "backups", `${id}-wine-10.0.tar.gz`)), "No backup before the update");
  assert.equal(/^wine=(.*)$/m.exec(settings())?.[1], wine);
  report.timings.upgrade = Date.now() - began;
  report.checks.upgrade = "passed";

  // Listed as installed in the prefix, and uninstalled: the program goes, and then the prefix.
  const listed = runner(["--prefix", id, "uninstaller", "--list"]);
  ok(listed, "Listing installed programs");
  // Wine's uninstaller prints its list on stderr.
  const product = /\{[0-9A-Fa-f-]+\}/.exec(`${listed.stdout}\n${listed.stderr}`)?.[0];
  assert.ok(product, `Wine does not list the installed program: ${listed.stderr.slice(-300)}`);
  // Quiet: `uninstaller --remove` asks first, as it does in the Desktop.
  ok(runner(["--prefix", id, "msiexec", "/x", product, "/qn"]), "The uninstall");
  settle();
  assert.ok(!existsSync(program), "The uninstaller left the program");
  ok(runner(["--remove", id]), "The removal");
  assert.deepEqual(prefixProcesses(prefix), [], "Processes of the prefix outlived it");
  report.checks.uninstall = "passed";
}

async function smoke(outputArg) {
  const outputRoot = resolve(outputArg);
  mkdirSync(outputRoot, { recursive: true });
  const output = mkdtempSync(join(outputRoot, "wine-"));
  const prefix = join(output, "prefix");
  const report = { ok: false, output, checks: {}, timings: {} };
  console.log(`Artifacts: ${output}`);
  const env = { ...process.env, WINEPREFIX: prefix, WINEDEBUG: "-all", WINEDLLOVERRIDES: "mscoree,mshtml=" };
  try {
    assert.ok(process.env.DISPLAY, "DISPLAY is not set");
    const version = run("wine", ["--version"], { env });
    assert.equal(version.status, 0, version.stderr);
    report.version = version.stdout.trim();

    // The prefix: the registry and drive C.
    let began = Date.now();
    const boot = run("wineboot", ["-i"], { env, timeout: 600_000 * scale });
    writeFileSync(join(output, "wineboot.log"), `${boot.stdout}\n${boot.stderr}`);
    if (boot.status !== 0) {
      // Once more in a new prefix, traced: which module failed and why, for the report.
      const traced = run("wineboot", ["-i"], {
        env: { ...env, WINEPREFIX: `${prefix}-traced`, WINEDEBUG: "err+all,warn+module,+loaddll" },
        timeout: 600_000 * scale,
      });
      report.tracedRetry = {
        status: traced.status ?? traced.error?.message,
        stderr: traced.stderr.split("\n").filter((line) => !/^\S+:trace:loaddll:.*: builtin$/.test(line)).slice(-80).join("\n"),
        memory: readFileSync("/proc/meminfo", "utf8").split("\n").slice(0, 4).join("; "),
      };
    }
    assert.equal(boot.status, 0, `wineboot failed (${boot.status ?? boot.error?.message}): ${boot.stderr.slice(-600)}`);
    run("wineserver", ["-w"], { env, timeout: 120_000 * scale });
    assert.ok(existsSync(join(prefix, "system.reg")) && existsSync(join(prefix, "drive_c", "windows")), "wineboot left no prefix");
    report.timings.wineboot = Date.now() - began;
    report.checks.wineboot = "passed";

    // Notepad opens a window, which closes it.
    began = Date.now();
    const notepad = spawn("wine", ["notepad"], { env, stdio: ["ignore", "ignore", "pipe"] });
    let stderr = "";
    notepad.stderr.on("data", (chunk) => { stderr = (stderr + chunk).slice(-4000); });
    const exited = new Promise((done) => notepad.once("exit", done));
    const window = await waitFor("the Notepad window", () => {
      const found = run("xdotool", ["search", "--onlyvisible", "--name", "Notepad"], { env }).stdout.trim().split("\n").filter(Boolean);
      return found[0] ?? null;
    }, 120_000);
    report.timings.notepadWindow = Date.now() - began;
    closeWindow(window, env);
    await Promise.race([exited, sleep(60_000 * scale).then(() => { throw new Error(`Notepad did not close: ${stderr.slice(-400)}`); })]);
    report.checks.notepad = "passed";

    // wineserver ends with the last program of the prefix, and nothing stays.
    const server = run("wineserver", ["-w"], { env, timeout: 120_000 * scale });
    assert.equal(server.status, 0, `wineserver -w: ${server.stderr}`);
    const left = await waitFor("the prefix's processes to exit", () => {
      const alive = prefixProcesses(prefix);
      return alive.length === 0 ? [] : null;
    }, 30_000).catch(() => prefixProcesses(prefix));
    assert.deepEqual(left, [], `Processes left behind: ${JSON.stringify(left)}`);
    report.checks.cleanup = "passed";

    // W4, with OpenShell's runner and a test installer (OPENSHELL_WINE_PROBE_MSI).
    if (process.env.OPENSHELL_WINE_PROBE_MSI) await installedApplication(process.env.OPENSHELL_WINE_PROBE_MSI, output, report, env);
    report.ok = true;
  } catch (error) {
    report.error = error instanceof Error ? error.message : String(error);
    for (const { pid } of prefixProcesses(prefix)) {
      try { process.kill(pid, "SIGKILL"); } catch { /* Gone. */ }
    }
  } finally {
    writeFileSync(join(output, "wine-smoke.json"), `${JSON.stringify(report, null, 2)}\n`);
    console.log(JSON.stringify(report, null, 2));
  }
  return report.ok;
}

const args = process.argv.slice(2);
if (args.length !== 1) {
  console.error("usage: wine-smoke.mjs <output-folder>");
  process.exitCode = 2;
} else if (!await smoke(args[0])) {
  process.exitCode = 1;
}
