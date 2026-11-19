// localforce's own API (the web UI's backend). Not part of Salesforce, so
// these only run against localforce: schema changes made through it must be
// visible to Salesforce clients straight away.

import { after, describe, test } from "node:test";
import { TAG, assert, connect, devApi, localforceOnly, q, rejects } from "./lib/sf.mjs";

const conn = await connect();
const object = `Lf_${TAG.slice(2)}__c`.replace(/[^A-Za-z0-9_]/g, "");

describe("schema changes from the UI", localforceOnly, () => {
  after(async () => {
    await devApi("DELETE", `/objects/${object}`);
  });

  test("a new object is described, writable and queryable at once", async () => {
    const res = await devApi("POST", "/objects", { name: object, label: "Gadget", label_plural: "Gadgets", name_field_format: "G-{000}" });
    assert.equal(res.status, 201, JSON.stringify(res.body));
    for (const field of [
      { name: "Account__c", type: "reference", reference_to: "Account", child_relationship_name: `${object.replace("__c", "")}s__r` },
      { name: "Size__c", type: "picklist", values: ["S", "M", "L"], restricted: true },
    ]) {
      const f = await devApi("POST", `/objects/${object}/fields`, field);
      assert.equal(f.status, 201, JSON.stringify(f.body));
    }

    const g = await conn.describeGlobal();
    assert.ok(g.sobjects.some((s) => s.name === object));
    const d = await conn.describe(object);
    assert.equal(d.label, "Gadget");
    assert.deepEqual(d.fields.find((f) => f.name === "Size__c").picklistValues.map((v) => v.value), ["S", "M", "L"]);

    const acct = await conn.sobject("Account").create({ Name: `${TAG} gadget owner` });
    const created = await conn.sobject(object).create({ Account__c: acct.id, Size__c: "M" });
    assert.equal(created.success, true);
    const [r] = await q(conn, `SELECT Name, Size__c, Account__r.Name FROM ${object} WHERE Id = '${created.id}'`);
    assert.equal(r.Name, "G-001");
    assert.equal(r.Account__r.Name, `${TAG} gadget owner`);
    const [parent] = await q(conn, `SELECT (SELECT Size__c FROM ${object.replace("__c", "")}s__r) FROM Account WHERE Id = '${acct.id}'`);
    assert.equal(parent[`${object.replace("__c", "")}s__r`].records[0].Size__c, "M");
    await rejects(conn.sobject(object).create({ Size__c: "XXL" }), "INVALID_OR_NULL_FOR_RESTRICTED_PICKLIST");
    await conn.sobject("Account").destroy(acct.id);
  });

  test("editing a field changes validation at once", async () => {
    const f = await devApi("PUT", `/objects/${object}/fields/Size__c`, { name: "Size__c", type: "picklist", values: ["S", "M", "L", "XL"], restricted: true });
    assert.equal(f.status, 204, JSON.stringify(f.body));
    const created = await conn.sobject(object).create({ Size__c: "XL" });
    assert.equal(created.success, true);
  });

  test("a deleted field is gone from describe and SOQL", async () => {
    const del = await devApi("DELETE", `/objects/${object}/fields/Size__c`);
    assert.equal(del.status, 204);
    const d = await conn.describe(object);
    assert.ok(!d.fields.some((f) => f.name === "Size__c"));
    await rejects(conn.query(`SELECT Size__c FROM ${object}`), "INVALID_FIELD");
  });

  test("fields from SFDX metadata can't be changed from the UI", async () => {
    const res = await devApi("DELETE", "/objects/Project__c/fields/Budget__c");
    assert.equal(res.status, 400);
    assert.match(res.body.error, /SFDX metadata/);
  });
});

describe("request log", localforceOnly, () => {
  test("records this client's calls, with the error code of failures", async () => {
    await conn.query("SELECT Id FROM User LIMIT 1");
    await conn.query("SELECT Nope FROM User").catch(() => {});
    const { body } = await devApi("GET", "/requests");
    const recent = body.slice(-20);
    assert.ok(recent.some((e) => e.method === "GET" && e.path.includes("/query") && e.status === 200));
    const failed = recent.findLast((e) => e.status === 400 && e.errorCode === "INVALID_FIELD" && e.path.includes("Nope"));
    assert.ok(failed, "the failed query is logged with its error code");
    const detail = await devApi("GET", `/requests/${failed.id}`);
    assert.match(detail.body.response_body, /No such column 'Nope'/);
  });
});

describe("SFDX export", localforceOnly, () => {
  test("exports an object's custom fields as metadata files", async () => {
    const { body } = await devApi("GET", "/objects/Project__c/metadata");
    const paths = body.files.map((f) => f.path);
    assert.ok(paths.includes("objects/Project__c/Project__c.object-meta.xml"));
    assert.ok(paths.includes("objects/Project__c/fields/Budget__c.field-meta.xml"));
    const budget = body.files.find((f) => f.path.endsWith("Budget__c.field-meta.xml")).content;
    assert.match(budget, /<type>Currency<\/type>/);
    assert.match(budget, /<precision>16<\/precision>/);
  });
});
