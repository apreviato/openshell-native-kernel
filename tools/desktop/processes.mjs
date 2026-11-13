// The process table and resource use of a process tree, from /proc, for the
// smokes that compare runtimes: shell-smoke.mjs (the CEF host) and smoke.mjs
// (Electron) measure the same way.

import { readdirSync, readFileSync } from "node:fs";

/** pid → { parent, cmdline } of every process. */
export function processes() {
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

/** A child of `parent` whose command line satisfies `predicate`. */
export function childOf(parent, predicate, table = processes()) {
  for (const [pid, info] of table) {
    if (info.parent === parent && predicate(info.cmdline)) return pid;
  }
  return null;
}

/** `root` and its descendants, without the subtrees of `excluded` pids. */
export function processTree(root, table = processes(), excluded = new Set()) {
  const members = new Set([root]);
  for (let grew = true; grew;) {
    grew = false;
    for (const [pid, info] of table) {
      if (!members.has(pid) && !excluded.has(pid) && members.has(info.parent)) {
        members.add(pid);
        grew = true;
      }
    }
  }
  return members;
}

/** Memory and processor time of a set of pids. */
export function resourceUseOf(members) {
  let rssKiB = 0;
  let pssKiB = 0;
  let pssAvailable = true;
  let cpuTicks = 0;
  for (const pid of members) {
    try {
      rssKiB += Number(readFileSync(`/proc/${pid}/status`, "utf8").match(/^VmRSS:\s+(\d+)/m)?.[1] ?? 0);
      // PSS divides shared pages, such as libcef.so or Electron's, among the processes.
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

/** Memory and processor time of `root` and its descendants. */
export function resourceUse(root) {
  return resourceUseOf(processTree(root));
}
