import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createSocket } from "node:dgram";
import { Resolver } from "node:dns/promises";
import { once } from "node:events";
import { readFileSync, watch } from "node:fs";
import { mkdtemp, open, readFile, rename, rm } from "node:fs/promises";
import { createServer, get } from "node:http";
import { createServer as createTlsServer, connect as connectTls } from "node:tls";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Worker } from "node:worker_threads";

const deadline = 8_000;
const fixture = "openshell-native";

async function processRoundTrip() {
  const child = spawn(process.execPath, ["-e", "process.stdin.pipe(process.stdout)"], {
    stdio: ["pipe", "pipe", "inherit"],
    timeout: deadline,
  });
  let output = "";
  child.stdout.setEncoding("utf8").on("data", (data) => { output += data; });
  child.stdin.end(fixture);
  assert.deepEqual(await once(child, "close"), [0, null]);
  assert.equal(output, fixture);
}

async function workerRoundTrip() {
  const worker = new Worker(
    'const {parentPort} = require("node:worker_threads"); parentPort.once("message", (data) => parentPort.postMessage(data));',
    { eval: true },
  );
  try {
    const reply = once(worker, "message", { signal: AbortSignal.timeout(deadline) });
    worker.postMessage(fixture);
    assert.deepEqual(await reply, [fixture]);
  } finally {
    await worker.terminate();
  }
}

async function fileEvents() {
  const dir = await mkdtemp(join(process.env.OPENSHELL_PROBE_TMP ?? tmpdir(), "openshell-node-"));
  try {
    const source = join(dir, "pending");
    const target = join(dir, "ready");
    const file = await open(source, "wx", 0o600);
    try {
      await file.writeFile(fixture);
      await file.sync();
    } finally {
      await file.close();
    }
    const watcher = watch(dir);
    try {
      let timer;
      const observed = [];
      const event = new Promise((resolve, reject) => {
        timer = setTimeout(() => reject(new Error(`File event timed out in ${dir}. Observed: ${JSON.stringify(observed)}`)), deadline);
        watcher.once("error", reject);
        watcher.on("change", (type, name) => {
          if (observed.length < 16) observed.push({ type, name });
          if (type === "rename" && name === "ready") resolve();
        });
      });
      try {
        await Promise.all([rename(source, target), event]);
        assert.equal(await readFile(target, "utf8"), fixture);
      } finally {
        clearTimeout(timer);
      }
    } finally {
      watcher.close();
    }
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

async function loopbackHttp() {
  const server = createServer((_request, response) => { response.end(fixture); });
  server.listen(0, "127.0.0.1");
  try {
    await once(server, "listening", { signal: AbortSignal.timeout(deadline) });
    const output = await new Promise((resolve, reject) => {
      const request = get({ host: "127.0.0.1", port: server.address().port, signal: AbortSignal.timeout(deadline) }, (response) => {
        let data = "";
        response.setEncoding("utf8").on("data", (chunk) => { data += chunk; });
        response.on("error", reject);
        response.on("end", () => resolve([response.statusCode, data]));
      });
      request.on("error", reject);
    });
    assert.deepEqual(output, [200, fixture]);
  } finally {
    const closed = new Promise((resolve) => server.close(resolve));
    server.closeAllConnections();
    await closed;
  }
}

// Answers every A query with one fixed address, as a local DNS fixture.
async function dnsFixture() {
  const name = "openshell.test";
  const address = "192.0.2.7";
  const server = createSocket("udp4");
  server.on("message", (query, peer) => {
    const question = query.subarray(12);
    const reply = Buffer.concat([
      query.subarray(0, 2),
      Buffer.from([0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]),
      question,
      Buffer.from([0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, ...address.split(".").map(Number)]),
    ]);
    server.send(reply, peer.port, peer.address);
  });
  server.bind(0, "127.0.0.1");
  try {
    await once(server, "listening", { signal: AbortSignal.timeout(deadline) });
    const resolver = new Resolver({ timeout: deadline / 2, tries: 1 });
    resolver.setServers([`127.0.0.1:${server.address().port}`]);
    assert.deepEqual(await resolver.resolve4(name), [address]);
  } finally {
    server.close();
  }
}

// Verifies a certificate chain and the host name against the fixture authority.
async function tlsFixture() {
  const directory = process.env.OPENSHELL_PROBE_TLS;
  assert.ok(directory, "OPENSHELL_PROBE_TLS must name the certificate fixture");
  const cert = readFileSync(join(directory, "cert.pem"));
  const key = readFileSync(join(directory, "key.pem"));
  const server = createTlsServer({ cert, key }, (socket) => { socket.end(fixture); });
  server.listen(0, "127.0.0.1");
  try {
    await once(server, "listening", { signal: AbortSignal.timeout(deadline) });
    const exchange = (options) => new Promise((resolve, reject) => {
      const socket = connectTls({ host: "127.0.0.1", port: server.address().port, ...options });
      let data = "";
      socket.setTimeout(deadline, () => socket.destroy(new Error("TLS exchange timed out")));
      socket.setEncoding("utf8").on("data", (chunk) => { data += chunk; });
      socket.on("error", reject);
      socket.on("end", () => resolve([socket.authorized, socket.getProtocol(), data]));
    });
    assert.deepEqual(await exchange({ ca: cert, servername: "localhost" }), [true, "TLSv1.3", fixture]);
    // The same server must be rejected without the fixture authority.
    await assert.rejects(exchange({ servername: "localhost" }), { code: "DEPTH_ZERO_SELF_SIGNED_CERT" });
  } finally {
    const closed = new Promise((resolve) => server.close(resolve));
    server.closeAllConnections?.();
    await closed;
  }
}

console.log(JSON.stringify({ runtime: "node", version: process.version, platform: process.platform }));
const probes = { processRoundTrip, workerRoundTrip, fileEvents, loopbackHttp, dnsFixture, tlsFixture };
for (const [name, run] of Object.entries(probes)) {
  try {
    await run();
    console.log(JSON.stringify({ probe: name, status: "passed" }));
  } catch (error) {
    process.exitCode = 1;
    console.log(JSON.stringify({ probe: name, status: "failed", error: error.message }));
  }
}
if (!process.exitCode) console.log("OPENSHELL_NATIVE_NODE_OK");
