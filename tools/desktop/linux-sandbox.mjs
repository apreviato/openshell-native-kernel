import { readdirSync, readFileSync } from "node:fs";

function readProcess(path) {
  try {
    return readFileSync(path, "utf8");
  } catch (error) {
    if (["EACCES", "EPERM", "ENOENT", "ESRCH"].includes(error.code)) return "";
    throw error;
  }
}

function field(status, name) {
  const value = status.match(new RegExp(`^${name}:[ \\t]+([0-9]+)`, "m"));
  return value ? Number(value[1]) : null;
}

// Zygote children rewrite their process title as one string that joins the switches
// with spaces, so /proc/[pid]/cmdline separates them by spaces instead of NUL.
export function commandLineArguments(cmdline) {
  return cmdline.split(/[\0 ]/).filter(Boolean);
}

export function inspectRenderer(pid, status, browser, argumentsList) {
  const seccomp = field(status, "Seccomp");
  const filters = field(status, "Seccomp_filters");
  const noNewPrivileges = field(status, "NoNewPrivs");
  const baseline = field(browser, "Seccomp");
  const baselineFilters = field(browser, "Seccomp_filters");
  const extraFilter = baseline === 0
    || (baselineFilters !== null && filters !== null && filters > baselineFilters);
  const disabled = argumentsList.some((arg) => /^--(?:no-sandbox|disable-seccomp-filter-sandbox|single-process)(?:=|$)/.test(arg));
  return {
    pid, seccomp, filters, noNewPrivileges, baseline, baselineFilters,
    filtered: seccomp === 2 && noNewPrivileges === 1 && extraFilter && !disabled,
  };
}

export function sandboxSample(browserPid) {
  const processes = new Map();
  for (const pid of readdirSync("/proc")) {
    if (!/^\d+$/.test(pid)) continue;
    const status = readProcess(`/proc/${pid}/status`);
    if (status) processes.set(Number(pid), status);
  }
  const browser = processes.get(browserPid);
  if (!browser) return [];

  const children = new Map();
  for (const [pid, status] of processes) {
    const parent = field(status, "PPid");
    if (!children.has(parent)) children.set(parent, []);
    children.get(parent).push(pid);
  }
  const renderers = [];
  const queue = [...(children.get(browserPid) ?? [])];
  for (const pid of queue) {
    queue.push(...(children.get(pid) ?? []));
    const argumentsList = commandLineArguments(readProcess(`/proc/${pid}/cmdline`));
    if (!argumentsList.includes("--type=renderer")) continue;
    renderers.push(inspectRenderer(pid, processes.get(pid), browser, argumentsList));
  }
  return renderers;
}
