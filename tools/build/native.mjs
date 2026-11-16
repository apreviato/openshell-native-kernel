import { spawn, spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, readdirSync } from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const secureBootKeys = process.env.OPENSHELL_SECURE_BOOT_KEYS ?? join(root, ".secureboot");
const lock = JSON.parse(readFileSync(join(root, "kernel", "config", "upstream.lock.json"), "utf8"));
const mediaActions = ["live-iso", "install-image"];
const bundleActions = ["desktop", "kiosk", "apps", "wine", "desktop-image", ...mediaActions];
const shellActions = ["graphics", "kiosk", "apps", "wine", "desktop-image", ...mediaActions];
const actions = ["doctor", "build", "smoke", "compat", "control", "sandbox", "baseline", "image", "graphics", ...bundleActions];
const usage = `Usage: node tools/build/native.mjs <${actions.filter((name) => !bundleActions.includes(name)).join("|")}>
       node tools/build/native.mjs <${bundleActions.join("|")}> <openshell-desktop-bundle>`;

function docker(args, { optional = false, inherit = false } = {}) {
  const result = spawnSync("docker", args, {
    encoding: "utf8",
    stdio: ["ignore", inherit ? "inherit" : "pipe", inherit ? "inherit" : "pipe"],
  });
  if (result.error?.code === "ENOENT") {
    throw new Error("Docker CLI unavailable. Install Docker and start its Linux engine.");
  }
  if (result.status !== 0 || result.error) {
    if (optional) return null;
    const detail = result.error?.message || result.stderr?.trim() || `exit status ${result.status}`;
    throw new Error(`docker ${args[0]} failed: ${detail}`);
  }
  return result.stdout?.trim() ?? "";
}

function checkEngine() {
  let server;
  try {
    server = docker(["version", "--format", "{{.Server.Os}}/{{.Server.Arch}}"]);
  } catch (error) {
    throw new Error(`Docker Linux engine is inaccessible to this session. ${error.message}`);
  }
  if (server !== "linux/amd64") {
    throw new Error(`The baseline requires a linux/amd64 Docker engine, found ${server || "unknown"}.`);
  }
  return server;
}

function resolveBuilder() {
  const requestedDigest = process.env.OPENSHELL_NATIVE_BUILDER_DIGEST ?? lock.builderDigest;
  if (!/^sha256:[0-9a-f]{64}$/.test(requestedDigest)) {
    throw new Error("The builder lock or OPENSHELL_NATIVE_BUILDER_DIGEST must provide a sha256 digest.");
  }
  const repository = lock.builderRepository;
  const reference = `${repository}@${requestedDigest}`;
  let info = docker(["image", "inspect", reference], { optional: true });
  if (info === null) {
    docker(["pull", "--platform", "linux/amd64", reference], { inherit: true });
    info = docker(["image", "inspect", reference]);
  }
  const [image] = JSON.parse(info);
  const repositoryName = repository.replace(/^docker\.io\//, "");
  const expectedPrefix = `${repositoryName}@`;
  const digestRef = image?.RepoDigests?.find((entry) => {
    const value = entry.replace(/^docker\.io\//, "");
    return value === `${expectedPrefix}${requestedDigest}`;
  });
  if (image?.Os !== "linux" || image?.Architecture !== "amd64" || !digestRef) {
    throw new Error(`Unable to identify the linux/amd64 registry digest for ${reference}.`);
  }
  return reference;
}

async function startContainer(container) {
  const child = spawn("docker", ["start", "--attach", container], { stdio: ["ignore", "inherit", "inherit"] });
  let interrupted = false;
  const stop = () => {
    interrupted = true;
    spawnSync("docker", ["stop", "--time", "5", container], { stdio: "ignore", timeout: 15_000 });
  };
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
  try {
    const status = await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", resolve);
    });
    if (interrupted) throw new Error("Native build interrupted.");
    const state = docker(["inspect", "--format", "{{.State.Status}} {{.State.ExitCode}}", container]);
    if (status !== 0 || state !== "exited 0") {
      throw new Error(`Native container failed (Docker status ${status}, state ${state}).`);
    }
  } finally {
    process.removeListener("SIGINT", stop);
    process.removeListener("SIGTERM", stop);
  }
}

// The root certificates Windows trusts, as PEM, for a build container behind
// an antivirus or proxy that inspects HTTPS (Norton's Web/Mail Shield, a
// company proxy): it re-signs every site with its own root, which Windows
// trusts and the builder image does not. run.sh takes from them only the
// root that signs what it sees, and only when its own bundle fails.
// Elsewhere null: a Linux or macOS host's roots are the image's.
function hostRoots() {
  if (process.platform !== "win32") return null;
  const script = "Get-ChildItem Cert:\\LocalMachine\\Root, Cert:\\CurrentUser\\Root | ForEach-Object { "
    + "'-----BEGIN CERTIFICATE-----'; [Convert]::ToBase64String($_.RawData, 'InsertLineBreaks'); '-----END CERTIFICATE-----' }";
  const result = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script],
    { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, windowsHide: true });
  if (result.status !== 0 || !result.stdout?.includes("BEGIN CERTIFICATE")) return null;
  const folder = mkdtempSync(join(tmpdir(), "openshell-native-roots-"));
  const file = join(folder, "host-roots.pem");
  writeFileSync(file, result.stdout.replace(/\r\n/g, "\n"));
  return { file, folder };
}

// Reads the identity of an OpenShell Desktop bundle and checks that it has the
// files that the Native session starts.
function readBundle(path) {
  const bundle = resolve(path);
  for (const file of ["BUILD.json", "hostd/openshell-hostd.mjs", "ui/index.html"]) {
    if (!existsSync(join(bundle, file))) throw new Error(`The Desktop bundle lacks ${file}: ${bundle}`);
  }
  const build = JSON.parse(readFileSync(join(bundle, "BUILD.json"), "utf8"));
  const hash = (...parts) => createHash("sha256").update(readFileSync(join(bundle, ...parts))).digest("hex");
  return { path: bundle, version: build.version, builtAt: build.builtAt, electron: build.electron,
    hostdSha256: hash("hostd", "openshell-hostd.mjs"), uiSha256: hash("ui", "index.html") };
}

async function run(action, bundlePath) {
  if (action === "doctor") {
    const engine = checkEngine();
    console.log(`Docker engine: ${engine}\nAsterinas: ${lock.tag} (${lock.commit})\nRust: ${lock.rust}`);
    return;
  }

  const outputRoot = join(root, "out");
  mkdirSync(outputRoot, { recursive: true });
  const output = mkdtempSync(join(outputRoot, "native-"));
  const reportPath = join(output, "run.json");
  const report = { action, upstream: lock.commit, image: null, status: "running", checks: {} };
  const bundle = bundleActions.includes(action) ? readBundle(bundlePath) : null;
  if (bundle) {
    const { path, ...identity } = bundle;
    report.bundle = identity;
  }
  const saveReport = () => writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  saveReport();
  console.log(`Artifacts: ${output}`);

  let container;
  try {
    checkEngine();
    const image = resolveBuilder();
    report.image = image;
    saveReport();
    console.log(`Builder: ${image}`);
    container = docker([
      "create", "--init", "--platform", "linux/amd64", "--entrypoint", "/bin/bash",
      // The Linux control of the graphics actions runs the Chromium sandbox,
      // which needs unprivileged user namespaces in the container.
      ...(shellActions.includes(action) && !mediaActions.includes(action)
        ? ["--security-opt", "seccomp=unconfined", "--security-opt", "apparmor=unconfined"] : []),
      image, "/tmp/openshell-native-run.sh", action,
      lock.repository, lock.tag, lock.commit, lock.rust, String(bundle?.version ?? ""),
    ]);
    docker(["cp", join(root, "tools", "qemu", "run.sh"), `${container}:/tmp/openshell-native-run.sh`]);
    const roots = hostRoots();
    if (roots) {
      try {
        docker(["cp", roots.file, `${container}:/tmp/openshell-native-host-roots.pem`]);
      } finally {
        rmSync(roots.folder, { recursive: true, force: true });
      }
    }
    docker(["cp", join(root, "userspace", "compatibility"), `${container}:/tmp/openshell-native-compat`]);
    docker(["cp", join(root, "distro", "ab"), `${container}:/tmp/openshell-native-ab`]);
    docker(["cp", join(root, "distro", "image"), `${container}:/tmp/openshell-native-image`]);
    // OpenShell Native's Secure Boot key: the same one for every build, so a
    // computer enrolls its certificate once (distro/image/secureboot.sh).
    if (existsSync(join(secureBootKeys, "openshell-native.key"))) {
      docker(["cp", secureBootKeys, `${container}:/tmp/openshell-native-secureboot`]);
    }
    for (const name of ["hostd-smoke.mjs", "cef-smoke.mjs", "shell-smoke.mjs", "session-smoke.mjs", "linux-sandbox.mjs", "apps-smoke.mjs", "wine-smoke.mjs"]) {
      docker(["cp", join(root, "tools", "desktop", name), `${container}:/tmp/openshell-native-compat/probes/${name}`]);
    }
    if (shellActions.includes(action)) {
      // The guest builds the Rust shell and the CEF host from these sources.
      // The recipe ignores a local Cargo target directory.
      docker(["cp", join(root, "userspace", "shell"), `${container}:/tmp/openshell-native-shell`]);
    }
    if (bundle) {
      docker(["cp", bundle.path, `${container}:/tmp/openshell-native-bundle`]);
    }
    if (action !== "control") {
      docker(["cp", join(root, "kernel", "patches"), `${container}:/tmp/openshell-native-patches`]);
    }
    if (action === "baseline") {
      for (const name of ["Cargo.toml", "Cargo.lock", "src"]) {
        docker(["cp", join(root, "userspace", "shell", name), `${container}:/tmp/openshell-native-compat/${name}`]);
      }
    }
    try {
      await startContainer(container);
    } catch (error) {
      docker(["cp", `${container}:/tmp/openshell-native-out/.`, output], { optional: true });
      keepSecureBootKey(output);
      throw error;
    }
    docker(["cp", `${container}:/tmp/openshell-native-out/.`, output]);
    keepSecureBootKey(output);
    if (mediaActions.includes(action)) {
      const pattern = action === "live-iso" ? /-amd64\.iso$/ : /-amd64\.img$/;
      const file = readdirSync(output).find((name) => name.startsWith("openshell-native-") && pattern.test(name));
      if (!file || !existsSync(join(output, `${file}.sha256`))) throw new Error("Media or checksum is missing.");
      report.media = file;
      console.log(`Media: ${join(output, file)}`);
    }
    report.status = "passed";
  } catch (error) {
    report.status = "failed";
    report.error = error.message;
    throw error;
  } finally {
    if (container && docker(["rm", "--force", container], { optional: true }) === null) {
      console.error(`Could not remove container ${container}.`);
    }
    const checksPath = join(output, "checks.tsv");
    if (existsSync(checksPath)) {
      const rows = readFileSync(checksPath, "utf8").trim().split(/\r?\n/);
      report.checks = Object.fromEntries(rows.filter((row) => row.includes("\t")).map((row) => row.split("\t", 2)));
    }
    saveReport();
  }
}

/**
 * The keys a build made go to .secureboot (git-ignored) the first time, and
 * never stay in out/: the Secure Boot key (distro/image/secureboot.sh) and the
 * release key of the update channel (distro/image/channel.sh).
 */
function keepSecureBootKey(output) {
  const made = join(output, "secureboot-key");
  if (!existsSync(made)) return;
  mkdirSync(secureBootKeys, { recursive: true });
  for (const name of readdirSync(made)) {
    if (existsSync(join(secureBootKeys, name))) continue;
    cpSync(join(made, name), join(secureBootKeys, name));
    if (name.endsWith(".key")) {
      console.log(`Key ${name} kept in ${secureBootKeys}: back it up. Media signed with another Secure Boot key need enrolling again, and installed systems take updates only from their release key.`);
    }
  }
  rmSync(made, { recursive: true, force: true });
}

const [action, ...extra] = process.argv.slice(2);
const expected = bundleActions.includes(action) ? 1 : 0;
if ((action === "--help" || action === "-h") && extra.length === 0) {
  console.log(usage);
} else if (extra.length !== expected || !actions.includes(action)) {
  console.error(usage);
  process.exitCode = 2;
} else {
  try {
    await run(action, extra[0]);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
