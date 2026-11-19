#!/usr/bin/env node
// Runs the jsforce suite: starts localforce (target/debug/localforce, or
// LOCALFORCE_BIN) on free ports with the conformance fixtures, runs every
// *.test.mjs against it, and stops it again.
//
// To run against a real org instead, set SF_INSTANCE_URL + SF_ACCESS_TOKEN, or
// SF_LOGIN_URL + SF_USERNAME + SF_PASSWORD + SF_CLIENT_ID + SF_CLIENT_SECRET.
// See tests/README.md.
//
// Arguments are passed to `node --test`:
//   node run.mjs --test-name-pattern="upsert"
//   node run.mjs jsforce.test.mjs

import { spawn, spawnSync } from "node:child_process";
import { createWriteStream, existsSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");
const fixtures = join(repo, "tests/conformance/fixtures");
const args = process.argv.slice(2);
const realOrg = !!(process.env.SF_INSTANCE_URL || process.env.SF_LOGIN_URL);

function freePort() {
  return new Promise((ok, fail) => {
    const s = createServer();
    s.unref();
    s.on("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => ok(port));
    });
  });
}

async function waitFor(url, ms) {
  const until = Date.now() + ms;
  for (;;) {
    try {
      if ((await fetch(url)).ok) return;
    } catch (_) {
      // not listening yet
    }
    if (Date.now() > until) throw new Error(`localforce didn't start: ${url}`);
    await new Promise((r) => setTimeout(r, 50));
  }
}

let server;
let dataDir;
const env = { ...process.env };

if (realOrg) {
  env.LF_TARGET = "salesforce";
  console.log(`Running against a real org: ${env.SF_INSTANCE_URL || env.SF_LOGIN_URL}\n`);
} else {
  const bin = process.env.LOCALFORCE_BIN || join(repo, "target/debug/localforce");
  if (!existsSync(bin)) {
    console.error(`No localforce binary at ${bin}. Run \`cargo build\` first, or set LOCALFORCE_BIN.`);
    process.exit(1);
  }
  const [port, tlsPort] = [await freePort(), await freePort()];
  dataDir = mkdtempSync(join(tmpdir(), "localforce-node-"));
  const logPath = join(here, "localforce.log");
  const log = createWriteStream(logPath);
  server = spawn(
    bin,
    [
      "--host", "127.0.0.1",
      "--port", String(port),
      "--tls-port", String(tlsPort),
      "--data-dir", dataDir,
      "--config", join(fixtures, "localforce.toml"),
      "--metadata", join(fixtures, "sfdx"),
      // Like Salesforce, refuse tokens localforce didn't issue.
      "--strict-sessions",
    ],
    { stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, NO_COLOR: "1" } },
  );
  server.stdout.pipe(log);
  server.stderr.pipe(log);
  server.on("exit", (code) => {
    if (code) console.error(`localforce exited with ${code}; see ${logPath}`);
  });
  const base = `http://127.0.0.1:${port}`;
  await waitFor(`${base}/services/data`, 30_000);
  Object.assign(env, {
    LF_TARGET: "localforce",
    LF_DEV_API: `${base}/__localforce/api`,
    SF_LOGIN_URL: base,
    SF_USERNAME: "admin@localforce.test",
    SF_PASSWORD: "any-password",
    SF_CLIENT_ID: "localforce-tests",
    SF_CLIENT_SECRET: "any-secret",
    LF_SECOND_USERNAME: "second@localforce.test",
  });
  console.log(`localforce is running on ${base} (log: ${logPath})\n`);
}

const stop = () => {
  if (server && !server.killed) server.kill();
  if (dataDir) rmSync(dataDir, { recursive: true, force: true });
};
process.on("SIGINT", () => {
  stop();
  process.exit(130);
});

const files = args.filter((a) => a.endsWith(".mjs"));
const flags = args.filter((a) => !a.endsWith(".mjs"));
const tests = files.length ? files : readdirSync(here).filter((f) => f.endsWith(".test.mjs")).sort();

const result = spawnSync(process.execPath, ["--test", "--test-concurrency=4", ...flags, ...tests], { cwd: here, stdio: "inherit", env });
stop();
process.exit(result.status ?? 1);
