// The Bulk APIs through jsforce: Bulk API 2.0 (`conn.bulk2`: ingest jobs
// with CSV uploads and per-row results, query jobs with locator paging) and
// Bulk API 1.0 (`conn.bulk`: XML jobs with CSV batches, and queries).

import { describe, test } from "node:test";
import { IS_LOCALFORCE, TAG, localforceOnly, assert, cleanup, connect, create, likeTag, q, raw, rejects, tagged } from "./lib/sf.mjs";

const conn = await connect();
const track = cleanup(conn);
// Localforce finishes jobs at once, so there's no need to wait between polls.
conn.bulk.pollInterval = IS_LOCALFORCE ? 50 : 2000;
conn.bulk.pollTimeout = 120_000;
conn.bulk2.pollInterval = IS_LOCALFORCE ? 50 : 2000;
conn.bulk2.pollTimeout = 120_000;

// Hard deletes need the "Bulk API Hard Delete" permission, which orgs don't grant by default.
const hardDelete = IS_LOCALFORCE ? {} : { skip: "needs the Bulk API Hard Delete permission" };

/** All the records from a jsforce record stream. */
function collect(stream) {
  return new Promise((ok, fail) => {
    const records = [];
    stream.on("record", (r) => records.push(r));
    stream.on("end", () => ok(records));
    stream.on("error", fail);
  });
}

const byName = (rows, field = "Name") => Object.fromEntries(rows.map((r) => [r[field], r]));

/** A unique external key. */
let keys = 0;
const key = (s = "") => `${TAG}-${++keys}${s}`;

/** A raw Bulk API 2.0 call returning the CSV (or JSON) body and headers. */
async function bulkRaw(method, path, body, contentType = "application/json") {
  const res = await fetch(`${conn.instanceUrl}/services/data/v${conn.version}/jobs${path}`, {
    method,
    headers: { authorization: `Bearer ${conn.accessToken}`, "content-type": contentType },
    body: body === undefined ? undefined : typeof body === "string" ? body : JSON.stringify(body),
  });
  const text = await res.text();
  let parsed = text;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch (_) {
    // CSV
  }
  return { status: res.status, headers: res.headers, body: parsed };
}

/** Wait for a 2.0 job to finish, and return its info. */
async function waitFor(kind, id) {
  for (let i = 0; i < 600; i++) {
    const res = await bulkRaw("GET", `/${kind}/${id}`);
    assert.equal(res.status, 200, JSON.stringify(res.body));
    if (["JobComplete", "Failed", "Aborted"].includes(res.body.state)) return res.body;
    await new Promise((r) => setTimeout(r, conn.bulk2.pollInterval));
  }
  throw new Error(`job ${id} didn't finish`);
}

/** Parse simple CSV (as Salesforce writes it: every value quoted) into rows. */
function parseCsv(text, delimiter = ",") {
  const rows = [];
  let row = [];
  let field = "";
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quoted) {
      if (c === '"' && text[i + 1] === '"') {
        field += '"';
        i++;
      } else if (c === '"') quoted = false;
      else field += c;
    } else if (c === '"') quoted = true;
    else if (c === delimiter) {
      row.push(field);
      field = "";
    } else if (c === "\n" || c === "\r") {
      if (c === "\r" && text[i + 1] === "\n") i++;
      row.push(field);
      rows.push(row);
      row = [];
      field = "";
    } else field += c;
  }
  if (field || row.length) rows.push([...row, field]);
  return rows;
}

describe("Bulk API 2.0 ingest (conn.bulk2)", () => {
  test("insert processes each row on its own: successful and failed results", async () => {
    const res = await conn.bulk2.loadAndWaitForResults({
      object: "Account",
      operation: "insert",
      input: [
        // jsforce takes the CSV's columns from the first record.
        { Name: tagged("b2 insert 1"), Tier__c: "Gold", NumberOfEmployees: 12, BillingCity: null },
        { Name: tagged("b2 insert 2, with a comma"), BillingCity: 'Says "hi", twice' },
        { Name: null, Tier__c: "Gold" },
      ],
    });
    track("Account", res.successfulResults.map((r) => r.sf__Id));
    assert.equal(res.successfulResults.length, 2);
    assert.equal(res.failedResults.length, 1);
    assert.equal(res.unprocessedRecords.length, 0);
    for (const r of res.successfulResults) {
      assert.match(r.sf__Id, /^001[a-zA-Z0-9]{15}$/);
      assert.equal(r.sf__Created, "true");
    }
    // The original columns come back with each result.
    assert.deepEqual(res.successfulResults.map((r) => r.Name).sort(), [tagged("b2 insert 1"), tagged("b2 insert 2, with a comma")]);
    const failed = res.failedResults[0];
    assert.equal(failed.sf__Id, "");
    assert.match(failed.sf__Error, /^REQUIRED_FIELD_MISSING:Required fields are missing: \[Name\]/);
    assert.equal(failed.Tier__c, "Gold");

    const rows = byName(await q(conn, `SELECT Name, Tier__c, NumberOfEmployees, BillingCity FROM Account WHERE ${likeTag()} AND Name LIKE '%b2 insert%'`));
    assert.equal(rows[tagged("b2 insert 1")].Tier__c, "Gold");
    assert.equal(rows[tagged("b2 insert 1")].NumberOfEmployees, 12);
    assert.equal(rows[tagged("b2 insert 2, with a comma")].BillingCity, 'Says "hi", twice');
  });

  test("the job's info counts processed and failed records", async () => {
    const job = conn.bulk2.createJob({ object: "Account", operation: "insert" });
    const opened = await job.open();
    assert.equal(opened.state, "Open");
    assert.equal(opened.object, "Account");
    assert.equal(opened.operation, "insert");
    assert.equal(opened.contentType, "CSV");
    assert.equal(opened.lineEnding, "LF");
    assert.equal(opened.columnDelimiter, "COMMA");
    await job.uploadData([
      { Name: tagged("b2 info 1"), Tier__c: "Gold" },
      { Name: tagged("b2 info 2"), Tier__c: "Gold" },
      { Name: null, Tier__c: "Gold" },
    ]);
    await job.close();
    await job.poll();
    const info = await job.check();
    assert.equal(info.state, "JobComplete");
    assert.equal(info.numberRecordsProcessed, 3);
    assert.equal(info.numberRecordsFailed, 1);
    assert.equal(info.jobType, "V2Ingest");
    const results = await job.getSuccessfulResults();
    track("Account", results.map((r) => r.sf__Id));

    const list = await bulkRaw("GET", "/ingest");
    assert.equal(list.status, 200);
    assert.equal(typeof list.body.done, "boolean");
    if (list.body.done) assert.ok(list.body.records.some((j) => j.id === job.id));

    await job.delete();
    const gone = await bulkRaw("GET", `/ingest/${job.id}`);
    assert.equal(gone.status, 404);
    assert.equal(gone.body[0].errorCode, "NOT_FOUND");
  });

  test("update: an empty value leaves a field alone, #N/A clears it", async () => {
    const [a, b] = await create(conn, track, "Account", [
      { Name: tagged("b2 update a"), Tier__c: "Gold", BillingCity: "keep me" },
      { Name: tagged("b2 update b"), Tier__c: "Gold", BillingCity: "keep me" },
    ]);
    const res = await conn.bulk2.loadAndWaitForResults({
      object: "Account",
      operation: "update",
      // jsforce writes null as #N/A, and BillingCity is an empty value.
      input: [
        { Id: a, Tier__c: "Silver", BillingCity: "" },
        { Id: b, Tier__c: null, BillingCity: "" },
      ],
    });
    assert.equal(res.failedResults.length, 0, JSON.stringify(res.failedResults));
    assert.deepEqual(res.successfulResults.map((r) => r.sf__Created), ["false", "false"]);
    assert.deepEqual(res.successfulResults.map((r) => r.sf__Id).sort(), [a, b].sort());
    const rows = byName(await q(conn, `SELECT Name, Tier__c, BillingCity FROM Account WHERE Id IN ('${a}', '${b}')`));
    assert.equal(rows[tagged("b2 update a")].Tier__c, "Silver");
    assert.equal(rows[tagged("b2 update a")].BillingCity, "keep me");
    assert.equal(rows[tagged("b2 update b")].Tier__c, null);
    assert.equal(rows[tagged("b2 update b")].BillingCity, "keep me");
  });

  test("update failures: a missing Id, a deleted record, a bad value", async () => {
    const [a, gone] = await create(conn, track, "Account", [{ Name: tagged("b2 bad update") }, { Name: tagged("b2 gone") }]);
    await conn.sobject("Account").destroy(gone);
    const res = await conn.bulk2.loadAndWaitForResults({
      object: "Account",
      operation: "update",
      input: `Id,NumberOfEmployees\n${a},lots\n${gone},3\n,4\n`,
    });
    assert.equal(res.successfulResults.length, 0);
    const errors = res.failedResults.map((r) => r.sf__Error.split(":")[0]).sort();
    assert.equal(errors.length, 3);
    assert.ok(errors.includes("ENTITY_IS_DELETED"), errors.join());
    assert.ok(errors.includes("MISSING_ARGUMENT"), errors.join());
  });

  test("upsert by an external id creates and updates, with sf__Created", async () => {
    const existing = key();
    const fresh = key();
    const id = await create(conn, track, "Account", { Name: tagged("b2 upsert old"), External_Key__c: existing });
    const res = await conn.bulk2.loadAndWaitForResults({
      object: "Account",
      operation: "upsert",
      externalIdFieldName: "External_Key__c",
      input: [
        { External_Key__c: existing, Name: tagged("b2 upsert updated") },
        { External_Key__c: fresh, Name: tagged("b2 upsert new") },
        { Name: tagged("b2 upsert no key") },
      ],
    });
    track("Account", res.successfulResults.map((r) => r.sf__Id));
    const ok = byName(res.successfulResults);
    assert.equal(ok[tagged("b2 upsert updated")].sf__Id, id);
    assert.equal(ok[tagged("b2 upsert updated")].sf__Created, "false");
    assert.equal(ok[tagged("b2 upsert new")].sf__Created, "true");
    assert.equal(res.failedResults.length, 1);
    assert.match(res.failedResults[0].sf__Error, /^MISSING_ARGUMENT:/);
    const rows = await q(conn, `SELECT Name FROM Account WHERE External_Key__c IN ('${existing}', '${fresh}') ORDER BY Name`);
    assert.deepEqual(rows.map((r) => r.Name), [tagged("b2 upsert new"), tagged("b2 upsert updated")]);
  });

  test("a relationship column looks up the parent by its external id", async () => {
    const k = key();
    const account = await create(conn, track, "Account", { Name: tagged("b2 parent"), External_Key__c: k });
    const res = await conn.bulk2.loadAndWaitForResults({
      object: "Contact",
      operation: "insert",
      input: `LastName,Account.External_Key__c\n${tagged("b2 child")},${k}\n${tagged("b2 orphan")},${k}-missing\n`,
    });
    track("Contact", res.successfulResults.map((r) => r.sf__Id));
    assert.equal(res.successfulResults.length, 1);
    assert.equal(res.successfulResults[0]["Account.External_Key__c"], k);
    assert.equal(res.failedResults.length, 1);
    assert.match(res.failedResults[0].sf__Error, /^INVALID_FIELD:Foreign key external ID: .*not found/);
    const [c] = await q(conn, `SELECT AccountId FROM Contact WHERE Id = '${res.successfulResults[0].sf__Id}'`);
    assert.equal(c.AccountId, account);
  });

  test("delete moves records to the recycle bin", async () => {
    const ids = await create(conn, track, "Account", [{ Name: tagged("b2 delete 1") }, { Name: tagged("b2 delete 2") }]);
    const res = await conn.bulk2.loadAndWaitForResults({ object: "Account", operation: "delete", input: ids.map((Id) => ({ Id })) });
    assert.equal(res.successfulResults.length, 2, JSON.stringify(res.failedResults));
    assert.equal((await q(conn, `SELECT Id FROM Account WHERE Id IN ('${ids.join("','")}')`)).length, 0);
    const binned = await conn.query(`SELECT Id, IsDeleted FROM Account WHERE Id IN ('${ids.join("','")}')`, { scanAll: true });
    assert.deepEqual(binned.records.map((r) => r.IsDeleted), [true, true]);
  });

  test("hardDelete skips the recycle bin", hardDelete, async () => {
    const ids = await create(conn, track, "Account", [{ Name: tagged("b2 hard 1") }, { Name: tagged("b2 hard 2") }]);
    const res = await conn.bulk2.loadAndWaitForResults({ object: "Account", operation: "hardDelete", input: ids.map((Id) => ({ Id })) });
    assert.equal(res.successfulResults.length, 2, JSON.stringify(res.failedResults));
    const binned = await conn.query(`SELECT Id FROM Account WHERE Id IN ('${ids.join("','")}')`, { scanAll: true });
    assert.equal(binned.records.length, 0);
  });

  test("another delimiter, CRLF line endings, and quoted values over several lines", async () => {
    const job = await bulkRaw("POST", "/ingest", { object: "Project__c", operation: "insert", columnDelimiter: "PIPE", lineEnding: "CRLF" });
    assert.equal(job.status, 200, JSON.stringify(job.body));
    assert.equal(job.body.columnDelimiter, "PIPE");
    const csv = `Name|Notes__c\r\n${tagged("b2 pipe 1")}|a, b\r\n"${tagged("b2 pipe | 2")}"|"line one\r\nline ""two"""\r\n`;
    assert.equal((await bulkRaw("PUT", `/ingest/${job.body.id}/batches`, csv, "text/csv")).status, 201);
    const closed = await bulkRaw("PATCH", `/ingest/${job.body.id}`, { state: "UploadComplete" });
    assert.equal(closed.status, 200);
    assert.equal(closed.body.state, "UploadComplete");
    const info = await waitFor("ingest", job.body.id);
    assert.equal(info.state, "JobComplete");
    assert.equal(info.numberRecordsProcessed, 2);
    const results = await bulkRaw("GET", `/ingest/${job.body.id}/successfulResults`);
    assert.match(results.headers.get("content-type"), /text\/csv/);
    assert.ok(results.body.includes("\r\n"));
    const rows = parseCsv(results.body, "|");
    assert.deepEqual(rows[0], ["sf__Id", "sf__Created", "Name", "Notes__c"]);
    track("Project__c", rows.slice(1).map((r) => r[0]));
    assert.deepEqual(rows.slice(1).map((r) => r[2]).sort(), [tagged("b2 pipe 1"), tagged("b2 pipe | 2")]);
    const [p] = await q(conn, `SELECT Notes__c FROM Project__c WHERE Name = '${tagged("b2 pipe | 2")}'`);
    assert.equal(p.Notes__c, 'line one\r\nline "two"');
  });

  test("an unknown column fails the whole job", async () => {
    const job = conn.bulk2.createJob({ object: "Account", operation: "insert" });
    await job.open();
    await job.uploadData(`Name,No_Such_Field__c\n${tagged("b2 bad column")},x\n`);
    await job.close();
    await assert.rejects(job.poll(), /Ingest job failed/);
    const info = await job.check();
    assert.equal(info.state, "Failed");
    assert.match(info.errorMessage, /No_Such_Field__c/);
    assert.equal((await q(conn, `SELECT Id FROM Account WHERE Name = '${tagged("b2 bad column")}'`)).length, 0);
  });

  test("aborting an open job; its data can't be uploaded or processed any more", async () => {
    const job = conn.bulk2.createJob({ object: "Account", operation: "insert" });
    await job.open();
    await job.uploadData([{ Name: tagged("b2 aborted") }]);
    await job.abort();
    assert.equal((await job.check()).state, "Aborted");
    const upload = await bulkRaw("PUT", `/ingest/${job.id}/batches`, "Name\nx\n", "text/csv");
    assert.ok(upload.status >= 400 && upload.status < 500, `status ${upload.status}`);
    const close = await bulkRaw("PATCH", `/ingest/${job.id}`, { state: "UploadComplete" });
    assert.ok(close.status >= 400 && close.status < 500, `status ${close.status}`);
    assert.equal((await q(conn, `SELECT Id FROM Account WHERE Name = '${tagged("b2 aborted")}'`)).length, 0);
    const unprocessed = await job.getUnprocessedRecords();
    assert.ok(Array.isArray(unprocessed));
    if (IS_LOCALFORCE) assert.deepEqual(unprocessed.map((r) => r.Name), [tagged("b2 aborted")]);
    await job.delete();
  });

  test("errors: an unknown object, an upsert without an external id, an unknown job", async () => {
    await rejects(conn.bulk2.createJob({ object: "No_Such_Object__c", operation: "insert" }).open(), ["INVALIDJOB", "NOT_FOUND", "INVALID_TYPE"]);
    await rejects(conn.bulk2.createJob({ object: "Account", operation: "upsert" }).open(), ["INVALIDJOB", "MISSING_ARGUMENT", "INVALID_FIELD"]);
    const bad = await bulkRaw("POST", "/ingest", { object: "Account", operation: "merge" });
    assert.equal(bad.status, 400);
    const missing = await bulkRaw("GET", "/ingest/750000000000000AAA");
    assert.equal(missing.status, 404);
    assert.equal(missing.body[0].errorCode, "NOT_FOUND");
    const noSession = await fetch(`${conn.instanceUrl}/services/data/v${conn.version}/jobs/ingest`, { headers: { authorization: "Bearer nope" } });
    assert.equal(noSession.status, 401);
    assert.equal((await noSession.json())[0].errorCode, "INVALID_SESSION_ID");
  });
});

describe("Bulk API 2.0 query (conn.bulk2)", () => {
  test("query returns CSV records with flattened parent fields and empty nulls", async () => {
    const k = key();
    const account = await create(conn, track, "Account", { Name: tagged("b2q parent"), External_Key__c: k });
    await create(conn, track, "Contact", [
      { LastName: tagged("b2q child"), AccountId: account, Email: "child@example.com" },
      { LastName: tagged("b2q orphan") },
    ]);
    const stream = await conn.bulk2.query(
      `SELECT Id, LastName, Email, Account.Name, Account.External_Key__c FROM Contact WHERE ${likeTag("LastName")} AND LastName LIKE '%b2q%' ORDER BY LastName`,
    );
    const rows = await collect(stream);
    assert.equal(rows.length, 2);
    assert.deepEqual(Object.keys(rows[0]), ["Id", "LastName", "Email", "Account.Name", "Account.External_Key__c"]);
    const child = byName(rows, "LastName")[tagged("b2q child")];
    assert.equal(child["Account.Name"], tagged("b2q parent"));
    assert.equal(child["Account.External_Key__c"], k);
    const orphan = byName(rows, "LastName")[tagged("b2q orphan")];
    assert.equal(orphan["Account.Name"], "");
    assert.equal(orphan.Email, "");
  });

  test("the query job: created UploadComplete, then JobComplete with a record count", async () => {
    await create(conn, track, "Account", [{ Name: tagged("b2q job 1") }, { Name: tagged("b2q job 2") }]);
    const job = await bulkRaw("POST", "/query", { operation: "query", query: `SELECT Id, Name FROM Account WHERE Name LIKE '${tagged("b2q job")}%'` });
    assert.equal(job.status, 200, JSON.stringify(job.body));
    assert.equal(job.body.operation, "query");
    assert.equal(job.body.object, "Account");
    assert.ok(["UploadComplete", "InProgress", "JobComplete"].includes(job.body.state), job.body.state);
    const info = await waitFor("query", job.body.id);
    assert.equal(info.state, "JobComplete");
    assert.equal(info.numberRecordsProcessed, 2);
    const list = await bulkRaw("GET", "/query");
    assert.equal(list.status, 200);
    if (list.body.done) assert.ok(list.body.records.some((j) => j.id === job.body.id));
    assert.equal((await bulkRaw("DELETE", `/query/${job.body.id}`)).status, 204);
    assert.equal((await bulkRaw("GET", `/query/${job.body.id}`)).status, 404);
  });

  test("results page by maxRecords and Sforce-Locator", async () => {
    const n = 1205;
    const res = await conn.bulk2.loadAndWaitForResults({
      object: "Account",
      operation: "insert",
      input: Array.from({ length: n }, (_, i) => ({ Name: tagged(`b2 page ${String(i).padStart(5, "0")}`) })),
    });
    track("Account", res.successfulResults.map((r) => r.sf__Id));
    assert.equal(res.successfulResults.length, n, JSON.stringify(res.failedResults.slice(0, 3)));

    const job = await bulkRaw("POST", "/query", { operation: "query", query: `SELECT Name FROM Account WHERE Name LIKE '${tagged("b2 page")}%'` });
    await waitFor("query", job.body.id);
    const names = [];
    let locator = "";
    const pages = [];
    do {
      const page = await bulkRaw("GET", `/query/${job.body.id}/results?maxRecords=500${locator ? `&locator=${locator}` : ""}`);
      assert.equal(page.status, 200);
      const rows = parseCsv(page.body);
      assert.deepEqual(rows[0], ["Name"]);
      assert.equal(Number(page.headers.get("sforce-numberofrecords")), rows.length - 1);
      pages.push(rows.length - 1);
      names.push(...rows.slice(1).map((r) => r[0]));
      locator = page.headers.get("sforce-locator");
    } while (locator !== "null");
    assert.equal(names.length, n);
    assert.equal(new Set(names).size, n);
    assert.ok(pages.length >= 3, `pages ${pages}`);
    assert.ok(pages.every((p) => p <= 500), `pages ${pages}`);

    // jsforce follows the locator too.
    const rows = await collect(await conn.bulk2.query(`SELECT Id FROM Account WHERE Name LIKE '${tagged("b2 page")}%'`));
    assert.equal(rows.length, n);
  });

  test("queryAll includes deleted records", async () => {
    const id = await create(conn, track, "Account", { Name: tagged("b2q deleted") });
    await conn.sobject("Account").destroy(id);
    const soql = `SELECT Id, IsDeleted FROM Account WHERE Id = '${id}'`;
    assert.equal((await collect(await conn.bulk2.query(soql))).length, 0);
    const all = await collect(await conn.bulk2.query(soql, { scanAll: true }));
    assert.equal(all.length, 1);
    assert.equal(all[0].IsDeleted, "true");
  });

  test("a bad query is refused when the job is created", async () => {
    const res = await bulkRaw("POST", "/query", { operation: "query", query: "SELECT No_Such_Field__c FROM Account" });
    assert.equal(res.status, 400);
    assert.equal(res.body[0].errorCode, "INVALID_FIELD");
    const sub = await bulkRaw("POST", "/query", { operation: "query", query: "SELECT Id, (SELECT Id FROM Contacts) FROM Account" });
    assert.equal(sub.status, 400);
  });
});

describe("Bulk API 1.0 (conn.bulk)", () => {
  test("load inserts records from CSV batches, with a result per record", async () => {
    const res = await conn.bulk.load("Account", "insert", [
      { Name: tagged("b1 insert 1"), Tier__c: "Gold", BillingCity: null },
      { Name: tagged("b1 insert 2"), BillingCity: 'a "city", with a comma' },
      { Name: null, Tier__c: "Silver" },
    ]);
    track("Account", res.map((r) => r.id));
    assert.equal(res.length, 3);
    assert.deepEqual(res.map((r) => r.success), [true, true, false]);
    assert.match(res[0].id, /^001[a-zA-Z0-9]{15}$/);
    assert.equal(res[0].created, true);
    assert.equal(res[2].id, null);
    assert.match(res[2].errors[0], /^REQUIRED_FIELD_MISSING:Required fields are missing: \[Name\]/);
    const [r] = await q(conn, `SELECT BillingCity FROM Account WHERE Id = '${res[1].id}'`);
    assert.equal(r.BillingCity, 'a "city", with a comma');
  });

  test("update and upsert by an external id", async () => {
    const k = key();
    const id = await create(conn, track, "Account", { Name: tagged("b1 update"), External_Key__c: k, Tier__c: "Gold" });
    const updated = await conn.bulk.load("Account", "update", [{ Id: id, Tier__c: null, NumberOfEmployees: 7 }]);
    assert.equal(updated[0].success, true, JSON.stringify(updated));
    assert.equal(updated[0].created, false);
    const [r] = await q(conn, `SELECT Tier__c, NumberOfEmployees FROM Account WHERE Id = '${id}'`);
    assert.equal(r.Tier__c, null);
    assert.equal(r.NumberOfEmployees, 7);

    const fresh = key();
    const upserted = await conn.bulk.load("Account", "upsert", { extIdField: "External_Key__c" }, [
      { External_Key__c: k, Name: tagged("b1 upserted") },
      { External_Key__c: fresh, Name: tagged("b1 upsert new") },
    ]);
    track("Account", upserted.map((x) => x.id));
    assert.deepEqual(upserted.map((x) => [x.success, x.created]), [[true, false], [true, true]]);
    assert.equal(upserted[0].id, id);
  });

  test("delete", async () => {
    const ids = await create(conn, track, "Account", [{ Name: tagged("b1 delete 1") }, { Name: tagged("b1 delete 2") }]);
    const res = await conn.bulk.load("Account", "delete", ids.map((Id) => ({ Id })));
    assert.deepEqual(res.map((r) => r.success), [true, true]);
    assert.deepEqual(res.map((r) => r.id).sort(), [...ids].sort());
    assert.equal((await q(conn, `SELECT Id FROM Account WHERE Id IN ('${ids.join("','")}')`)).length, 0);
  });

  test("hardDelete", hardDelete, async () => {
    const ids = await create(conn, track, "Account", [{ Name: tagged("b1 hard 1") }]);
    const res = await conn.bulk.load("Account", "hardDelete", ids.map((Id) => ({ Id })));
    assert.equal(res[0].success, true);
    assert.equal((await conn.query(`SELECT Id FROM Account WHERE Id = '${ids[0]}'`, { scanAll: true })).records.length, 0);
  });

  test("query streams CSV results", async () => {
    const k = key();
    const account = await create(conn, track, "Account", { Name: tagged("b1q parent"), External_Key__c: k });
    await create(conn, track, "Contact", [
      { LastName: tagged("b1q child"), AccountId: account },
      { LastName: tagged("b1q orphan") },
    ]);
    const rows = await collect(
      await conn.bulk.query(`SELECT Id, LastName, Account.Name FROM Contact WHERE LastName LIKE '${tagged("b1q")}%' ORDER BY LastName`),
    );
    assert.equal(rows.length, 2);
    const child = byName(rows, "LastName")[tagged("b1q child")];
    assert.equal(child["Account.Name"], tagged("b1q parent"));
    assert.equal(byName(rows, "LastName")[tagged("b1q orphan")]["Account.Name"], "");
  });

  test("job and batch info", async () => {
    const job = conn.bulk.createJob("Account", "insert");
    const batch = job.createBatch();
    const done = new Promise((ok, fail) => {
      batch.on("response", ok);
      batch.on("error", fail);
    });
    batch.on("queue", () => batch.poll(conn.bulk.pollInterval, conn.bulk.pollTimeout));
    batch.execute([{ Name: tagged("b1 info") }]);
    const results = await done;
    track("Account", results.map((r) => r.id));
    const info = await job.check();
    assert.equal(info.state, "Open");
    assert.equal(info.object, "Account");
    assert.equal(info.operation, "insert");
    assert.equal(info.contentType, "CSV");
    const batches = await job.list();
    assert.equal(batches.length, 1);
    assert.equal(batches[0].state, "Completed");
    assert.equal(batches[0].numberRecordsProcessed, "1");
    const closed = await job.close();
    assert.equal(closed.state, "Closed");
    assert.equal(closed.numberRecordsProcessed, "1");
  });

  test("XML jobs: XML records in, XML results out", async () => {
    const ns = "http://www.force.com/2009/06/asyncapi/dataload";
    const call = async (method, path, body) => {
      const res = await fetch(`${conn.instanceUrl}/services/async/${conn.version}${path}`, {
        method,
        headers: { "x-sfdc-session": conn.accessToken, "content-type": "application/xml; charset=UTF-8" },
        body,
      });
      return { status: res.status, text: await res.text() };
    };
    const tagText = (xml, name) => xml.match(new RegExp(`<${name}>([^<]*)</${name}>`))?.[1];
    const job = await call("POST", "/job", `<?xml version="1.0" encoding="UTF-8"?><jobInfo xmlns="${ns}"><operation>insert</operation><object>Account</object><contentType>XML</contentType></jobInfo>`);
    assert.equal(job.status, 201, job.text);
    assert.equal(tagText(job.text, "contentType"), "XML");
    const jobId = tagText(job.text, "id");
    const records = `<?xml version="1.0" encoding="UTF-8"?><sObjects xmlns="${ns}" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
      <sObject><Name>${tagged("b1 xml &amp; co")}</Name><NumberOfEmployees>3</NumberOfEmployees><Tier__c xsi:nil="true"/></sObject>
      <sObject><NumberOfEmployees>4</NumberOfEmployees></sObject>
    </sObjects>`;
    const batch = await call("POST", `/job/${jobId}/batch`, records);
    assert.equal(batch.status, 201, batch.text);
    const batchId = tagText(batch.text, "id");
    let state = tagText(batch.text, "state");
    for (let i = 0; i < 100 && !["Completed", "Failed"].includes(state); i++) {
      await new Promise((r) => setTimeout(r, conn.bulk.pollInterval));
      state = tagText((await call("GET", `/job/${jobId}/batch/${batchId}`)).text, "state");
    }
    assert.equal(state, "Completed");
    const results = await call("GET", `/job/${jobId}/batch/${batchId}/result`);
    assert.match(results.text, new RegExp(`<results xmlns="${ns}">`));
    const id = tagText(results.text, "id");
    track("Account", id);
    assert.match(results.text, /<success>true<\/success><created>true<\/created>/);
    assert.match(results.text, /<statusCode>REQUIRED_FIELD_MISSING<\/statusCode>/);
    const request = await call("GET", `/job/${jobId}/batch/${batchId}/request`);
    assert.match(request.text, /<NumberOfEmployees>4<\/NumberOfEmployees>/);
    const closed = await call("POST", `/job/${jobId}`, `<?xml version="1.0" encoding="UTF-8"?><jobInfo xmlns="${ns}"><state>Closed</state></jobInfo>`);
    assert.equal(tagText(closed.text, "state"), "Closed");
    const late = await call("POST", `/job/${jobId}/batch`, records);
    assert.equal(late.status, 400);
    assert.equal(tagText(late.text, "exceptionCode"), "InvalidJobState");
    const [r] = await q(conn, `SELECT Name, NumberOfEmployees FROM Account WHERE Id = '${id}'`);
    assert.deepEqual([r.Name, r.NumberOfEmployees], [tagged("b1 xml & co"), 3]);
  });

  test("errors: an unknown object, a bad session", async () => {
    await rejects(conn.bulk.load("No_Such_Object__c", "insert", [{ Name: "x" }]), "InvalidJob");
    const res = await fetch(`${conn.instanceUrl}/services/async/${conn.version}/job`, {
      method: "POST",
      headers: { "x-sfdc-session": "not-a-session", "content-type": "application/xml" },
      body: `<?xml version="1.0" encoding="UTF-8"?><jobInfo xmlns="http://www.force.com/2009/06/asyncapi/dataload"><operation>insert</operation><object>Account</object></jobInfo>`,
    });
    assert.ok([400, 401].includes(res.status), `status ${res.status}`);
    assert.match(await res.text(), /<exceptionCode>InvalidSessionId<\/exceptionCode>/);
  });
});

// Salesforce updates the remaining count lazily.
test("bulk calls count against the API limit", localforceOnly, async () => {
  const before = (await raw(conn, "GET", "/limits")).body.DailyApiRequests.Remaining;
  await bulkRaw("GET", "/ingest");
  const after = (await raw(conn, "GET", "/limits")).body.DailyApiRequests.Remaining;
  assert.ok(after < before, `${before} -> ${after}`);
});
