import assert from "node:assert/strict";
import { test } from "node:test";
import { commandLineArguments, inspectRenderer } from "./linux-sandbox.mjs";

const renderer = "Seccomp:\t2\nSeccomp_filters:\t2\nNoNewPrivs:\t1\n";
const outer = "Seccomp:\t2\nSeccomp_filters:\t1\n";
const flags = ["openshell-cef-host", "--type=renderer"];

test("proves a filter added by Chromium", () => {
  assert.equal(inspectRenderer(123, renderer, "Seccomp:\t0\n", flags).filtered, true);
  assert.equal(inspectRenderer(123, renderer, outer, flags).filtered, true);
});

test("rejects inherited filtering and missing evidence", () => {
  assert.equal(inspectRenderer(123, renderer, renderer, flags).filtered, false);
  assert.equal(inspectRenderer(123, renderer, "", flags).filtered, false);
  assert.equal(inspectRenderer(123, renderer.replace("NoNewPrivs:\t1", "NoNewPrivs:\t0"), outer, flags).filtered, false);
});

test("reads switches from rewritten process titles", () => {
  assert.deepEqual(commandLineArguments("openshell-cef-host\0--type=renderer\0"), flags);
  assert.deepEqual(commandLineArguments("openshell-cef-host --type=renderer --no-sandbox"),
    [...flags, "--no-sandbox"]);
});

test("rejects startup flags that disable the process sandbox", () => {
  for (const flag of ["--no-sandbox", "--disable-seccomp-filter-sandbox=1", "--single-process"]) {
    assert.equal(inspectRenderer(123, renderer, outer, [...flags, flag]).filtered, false);
  }
});
