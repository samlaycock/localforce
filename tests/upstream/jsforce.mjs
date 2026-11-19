#!/usr/bin/env node
// Runs jsforce's own integration tests (the ones its maintainers run against a
// real scratch org) against localforce.
//
// It fetches jsforce at a pinned commit into tests/upstream/.cache, builds it,
// converts its test package's custom objects to SFDX source, starts localforce
// (target/debug/localforce, or LOCALFORCE_BIN) with them, loads the data
// jsforce's org setup loads (25,005 BigTable__c rows) plus sample data like a
// Developer Edition org's, and runs the test files for the APIs localforce
// emulates. Tests of features localforce doesn't have (approval processes,
// Apex, Tooling API writes) are listed in EXPECTED_FAILURES: the run fails only
// on other failures, or when an expected failure starts passing.
//
//   node tests/upstream/jsforce.mjs
//   node tests/upstream/jsforce.mjs test/query.test.ts   # one file

import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const JSFORCE_COMMIT = "d0837cb343ddd44bcf59d8b0cee02175e2ba8c63"; // 3.13.1, 2026-10-27

const TEST_FILES = [
  "test/body-size.test.ts",
  "test/bulk.test.ts",
  "test/bulk2.test.ts",
  "test/cache.test.ts",
  "test/connection-crud.test.ts",
  "test/connection-meta.test.ts",
  "test/connection-session.test.ts",
  "test/http-api.test.ts",
  "test/oauth2.test.ts",
  "test/process.test.ts",
  "test/query.test.ts",
  "test/schema.test.ts",
  "test/soap.test.ts",
  "test/sobject.test.ts",
  "test/tooling.test.ts",
];
// Not run: analytics, chatter, metadata, apex, streaming and quick actions
// (APIs localforce doesn't have), and the browser and unit-only files.

/** `file › test title` of tests that need something localforce doesn't emulate. */
const EXPECTED_FAILURES = new Set([
  // Approval processes and process rules.
  "process.test.ts › should retrieve all process rules and list process rules",
  "process.test.ts › should trigger process rule and trigger process rules",
  "process.test.ts › should retrieve all approval process definitions and list approval process definitions",
  "process.test.ts › should submit approval process and confirm approval request submitted",
  "process.test.ts › should approve requested approval request",
  "process.test.ts › should reject approval request",
  // Apex and Tooling API writes.
  "tooling.test.ts › should execute anonymous apex and execute successfully",
  "tooling.test.ts › should run tests asynchronously",
  "tooling.test.ts › should run tests synchronously",
  "tooling.test.ts › should get completions and return completions",
  "tooling.test.ts › can create static resource using application/json content type",
  "tooling.test.ts › can create static resource using multipart/form-data content type with a buffer",
  "tooling.test.ts › can create static resource using multipart/form-data content type with a base64 encoded string",
  "tooling.test.ts › can download a static resource binary blob and properly interpret the results",
  "tooling.test.ts › should create debuglevel and get created obj",
  "tooling.test.ts › should retrieve debuglevel and return a record",
  "tooling.test.ts › should update debuglevel, get successful result, and retrieve the updated record",
  "tooling.test.ts › should delete debuglevel, get successful results, and not get any records",
  // The ActivityHistories / OpenActivities child relationships.
  "schema.test.ts › should query",
]);

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../..");
const cache = join(here, ".cache");
const jsforceDir = join(cache, `jsforce-${JSFORCE_COMMIT.slice(0, 12)}`);
const run = (cmd, args, opts = {}) => execFileSync(cmd, args, { stdio: "inherit", ...opts });

// 1. jsforce, built.
if (!existsSync(join(jsforceDir, "lib"))) {
  mkdirSync(cache, { recursive: true });
  rmSync(jsforceDir, { recursive: true, force: true });
  run("git", ["clone", "--quiet", "--filter=blob:none", "https://github.com/jsforce/jsforce", jsforceDir]);
  run("git", ["-C", jsforceDir, "checkout", "--quiet", JSFORCE_COMMIT]);
  run("npm", ["ci", "--ignore-scripts", "--no-audit", "--no-fund"], { cwd: jsforceDir });
  run("npx", ["tsc", "-p", "."], { cwd: jsforceDir });
}

// 2. Its test package's objects, as SFDX source.
const work = mkdtempSync(join(tmpdir(), "localforce-jsforce-"));
const project = join(work, "project");
const objects = join(jsforceDir, "test/package/JSforceTestSuite/objects");
const ns = 'xmlns="http://soap.sforce.com/2006/04/metadata"';
for (const file of readdirSync(objects).filter((f) => f.endsWith(".object"))) {
  const name = file.replace(/\.object$/, "");
  const xml = readFileSync(join(objects, file), "utf8");
  const dir = join(project, "force-app/main/default/objects", name);
  mkdirSync(join(dir, "fields"), { recursive: true });
  for (const [, body] of xml.matchAll(/<fields>([\s\S]*?)<\/fields>/g)) {
    const full = body.match(/<fullName>(.*?)<\/fullName>/)[1];
    writeFileSync(join(dir, "fields", `${full}.field-meta.xml`), `<?xml version="1.0" encoding="UTF-8"?>\n<CustomField ${ns}>${body}</CustomField>\n`);
  }
  writeFileSync(join(dir, `${name}.object-meta.xml`), xml.replace(/\s*<(fields|listViews|webLinks|actionOverrides)>[\s\S]*?<\/\1>/g, ""));
}
writeFileSync(join(project, "sfdx-project.json"), '{"packageDirectories":[{"path":"force-app","default":true}]}\n');

// 3. localforce. jsforce checks the identity URL is https, so it uses the TLS port.
const freePort = () =>
  new Promise((ok, fail) => {
    const s = createServer();
    s.on("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => ok(port));
    });
  });
const bin = process.env.LOCALFORCE_BIN || join(repo, "target/debug/localforce");
if (!existsSync(bin)) {
  console.error(`No localforce binary at ${bin}. Run \`cargo build\` first, or set LOCALFORCE_BIN.`);
  process.exit(1);
}
const [port, tlsPort] = [await freePort(), await freePort()];
const dataDir = join(work, "data");
const server = spawn(
  bin,
  ["--host", "127.0.0.1", "--port", String(port), "--tls-port", String(tlsPort), "--data-dir", dataDir, "--config", "/dev/null", "--metadata", project, "--strict-sessions"],
  { stdio: ["ignore", "ignore", "inherit"] },
);
const stop = () => {
  server.kill();
  rmSync(work, { recursive: true, force: true });
};
const http = `http://127.0.0.1:${port}`;
for (let i = 0; ; i++) {
  try {
    if ((await fetch(`${http}/services/data`)).ok) break;
  } catch {}
  if (i > 200) throw new Error("localforce didn't start");
  await new Promise((r) => setTimeout(r, 50));
}

// 4. The data jsforce's org setup loads, and sample data like a new Developer Edition org's.
const { access_token: token } = await (
  await fetch(`${http}/services/oauth2/token`, {
    method: "POST",
    body: new URLSearchParams({ grant_type: "password", client_id: "x", client_secret: "x", username: "admin@localforce.dev", password: "x" }),
  })
).json();
const api = async (method, path, body, type = "application/json") => {
  const r = await fetch(`${http}/services/data/v62.0${path}`, {
    method,
    headers: { Authorization: `Bearer ${token}`, "Content-Type": type },
    body: body === undefined ? undefined : type === "application/json" ? JSON.stringify(body) : body,
  });
  if (!r.ok) throw new Error(`${method} ${path}: ${r.status} ${await r.text()}`);
  return r.status === 204 ? null : r.headers.get("content-type")?.includes("json") ? r.json() : r.text();
};
const { user_id: userId } = await (await fetch(`${http}/services/oauth2/userinfo`, { headers: { Authorization: `Bearer ${token}` } })).json();
const job = await api("POST", "/jobs/ingest", { object: "BigTable__c", operation: "insert", contentType: "CSV" });
await api("PUT", `/jobs/ingest/${job.id}/batches`, "OwnerId\n" + `${userId}\n`.repeat(25005), "text/csv");
await api("PATCH", `/jobs/ingest/${job.id}`, { state: "UploadComplete" });
const sample = [
  ["Edge Communications", "TX", "Customer - Direct"],
  ["Burlington Textiles Corp of America", "NC", "Customer - Direct"],
  ["Pyramid Construction Inc.", null, "Customer - Channel"],
  ["Dickenson plc", "KS", "Customer - Channel"],
  ["Grand Hotels & Resorts Ltd", "IL", "Customer - Direct"],
  ["United Oil & Gas Corp.", "NY", "Customer - Direct"],
  ["Express Logistics and Transport", "OR", "Customer - Channel"],
  ["University of Arizona", "AZ", "Customer - Direct"],
  ["United Oil & Gas, UK", null, "Customer - Direct"],
  ["United Oil & Gas, Singapore", null, "Customer - Direct"],
  ["GenePoint", "CA", "Customer - Channel"],
  ["sForce", "CA", null],
];
await api("POST", "/composite/tree/Account", {
  records: sample.map(([name, state, type], i) => ({
    attributes: { type: "Account", referenceId: `a${i}` },
    Name: name,
    BillingState: state,
    Type: type,
    Contacts: { records: [{ attributes: { type: "Contact", referenceId: `c${i}` }, FirstName: "Sample", LastName: `Contact ${i}`, Email: `contact${i}@example.com` }] },
    Cases: { records: [{ attributes: { type: "Case", referenceId: `k${i}` }, Subject: `Sample case ${i}` }] },
    // Like the real sample data, sForce has no opportunities.
    ...(name === "sForce"
      ? {}
      : {
          Opportunities: {
            records: [
              {
                attributes: { type: "Opportunity", referenceId: `o${i}` },
                Name: `${name} Deal`,
                StageName: i % 2 ? "Prospecting" : "Closed Won",
                CloseDate: `2026-${String((i % 12) + 1).padStart(2, "0")}-15`,
                Amount: 10000 * (i + 1),
              },
            ],
          },
        }),
  })),
});
for (let i = 0; i < 20; i++) {
  await api("POST", "/sobjects/Lead", { FirstName: "Sample", LastName: `Lead ${i}`, Company: `Company ${i}` });
}

// 5. The tests.
const results = join(work, "results.json");
const files = process.argv.slice(2).length ? process.argv.slice(2) : TEST_FILES;
try {
  execFileSync("npx", ["jest", "--forceExit", "--json", `--outputFile=${results}`, ...files], {
    cwd: jsforceDir,
    stdio: ["ignore", "ignore", "ignore"],
    env: {
      ...process.env,
      NODE_EXTRA_CA_CERTS: join(dataDir, "localforce-cert.pem"),
      SF_LOGIN_URL: `https://localhost:${tlsPort}`,
      SF_ACCESS_TOKEN: token,
      SF_USERNAME: "admin@localforce.dev",
      SF_PASSWORD: "x",
      SF_OAUTH2_CLIENT_ID: "x",
      SF_OAUTH2_CLIENT_SECRET: "x",
    },
  });
} catch {
  // Failures are reported below.
}
const report = JSON.parse(readFileSync(results, "utf8"));
stop();

const unexpected = [];
const fixed = [];
let [passed, failed, skipped] = [0, 0, 0];
for (const file of report.testResults) {
  const name = file.name.split("/").pop();
  if (file.assertionResults.length === 0 && file.message) {
    unexpected.push(`${name}: ${file.message.split("\n")[0]}`);
  }
  for (const t of file.assertionResults) {
    const key = `${name} › ${t.title}`;
    if (t.status === "passed") passed++;
    else if (t.status === "failed") failed++;
    else skipped++;
    if (t.status === "failed" && !EXPECTED_FAILURES.has(key)) {
      const why = t.failureMessages.join("\n").replace(/\x1b\[[0-9;]*m/g, "").split("\n").find((l) => l.trim()) ?? "";
      unexpected.push(`${key}\n    ${why.trim().slice(0, 200)}`);
    }
    if (t.status === "passed" && EXPECTED_FAILURES.has(key)) fixed.push(key);
  }
}
console.log(`jsforce ${JSFORCE_COMMIT.slice(0, 12)}: ${passed} passed, ${failed} failed (${failed - unexpected.length} expected), ${skipped} skipped`);
for (const u of unexpected) console.log(`  unexpected failure: ${u}`);
for (const f of fixed) console.log(`  now passes, remove it from EXPECTED_FAILURES: ${f}`);
process.exit(unexpected.length || fixed.length ? 1 : 0);
