// jsforce's own APIs, which the shared spec can't express: login flows,
// identity, limitInfo, paging helpers (autoFetch, maxFetch, record events,
// queryMore), the query builder, describe caching and allowRecursive batches.

import { before, describe, test } from "node:test";
import jsforce from "jsforce";
import { API_VERSION, IS_LOCALFORCE, TAG, assert, cleanup, connect, create, hasPassword, likeTag, pluck, raw, rejects, tagged } from "./lib/sf.mjs";

const needsPassword = hasPassword ? {} : { skip: "needs SF_USERNAME and SF_PASSWORD" };
const conn = await connect();
const track = cleanup(conn);

describe("login", () => {
  test("OAuth 2.0 username-password flow returns a session for the org and user", needsPassword, async () => {
    const c = await connect();
    assert.ok(c.accessToken);
    assert.match(c.instanceUrl, /^https?:\/\//);
    assert.match(c.userInfo.id, /^005[a-zA-Z0-9]{15}$/);
    assert.match(c.userInfo.organizationId, /^00D[a-zA-Z0-9]{15}$/);
  });

  test("identity() describes the user", needsPassword, async () => {
    const c = await connect();
    const id = await c.identity();
    assert.equal(id.user_id, c.userInfo.id);
    assert.equal(id.organization_id, c.userInfo.organizationId);
    assert.equal(id.username.toLowerCase(), process.env.SF_USERNAME.toLowerCase());
  });

  test("an unknown username is invalid_grant", needsPassword, async () => {
    const c = new jsforce.Connection({
      oauth2: { loginUrl: process.env.SF_LOGIN_URL, clientId: process.env.SF_CLIENT_ID, clientSecret: process.env.SF_CLIENT_SECRET },
      version: API_VERSION,
    });
    await rejects(c.login("nobody-at-all@example.invalid", "x"), "invalid_grant", { message: "authentication failure" });
  });

  test("SOAP login finds the instance from serverUrl", needsPassword, async () => {
    const c = new jsforce.Connection({ loginUrl: process.env.SF_LOGIN_URL, version: API_VERSION });
    const info = await c.login(process.env.SF_USERNAME, process.env.SF_PASSWORD);
    assert.match(info.id, /^005/);
    assert.match(info.organizationId, /^00D/);
    assert.equal((await c.query(`SELECT Id FROM User WHERE Id = '${info.id}'`)).totalSize, 1);
  });

  test("a bad SOAP login is an INVALID_LOGIN fault", needsPassword, async () => {
    const c = new jsforce.Connection({ loginUrl: process.env.SF_LOGIN_URL, version: API_VERSION });
    await assert.rejects(c.login("nobody-at-all@example.invalid", "x"), /INVALID_LOGIN/);
  });

  test("logout() revokes the session, and only that one", needsPassword, async () => {
    const other = await connect();
    const c = await connect();
    assert.notEqual(c.accessToken, other.accessToken);
    const stale = new jsforce.Connection({ instanceUrl: c.instanceUrl, accessToken: c.accessToken, version: API_VERSION });
    await c.logout();
    await rejects(stale.query("SELECT Id FROM User LIMIT 1"), "INVALID_SESSION_ID");
    assert.equal((await other.query("SELECT Id FROM User LIMIT 1")).records.length, 1);
  });

  test("the client_credentials grant returns a session", IS_LOCALFORCE ? {} : { skip: "needs a connected app with a run-as user" }, async () => {
    const res = await fetch(`${process.env.SF_LOGIN_URL}/services/oauth2/token`, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({ grant_type: "client_credentials", client_id: "x", client_secret: "y" }),
    });
    const token = await res.json();
    assert.equal(res.status, 200);
    assert.equal(token.token_type, "Bearer");
    const c = new jsforce.Connection({ instanceUrl: token.instance_url, accessToken: token.access_token, version: API_VERSION });
    assert.equal((await c.query("SELECT Id FROM User LIMIT 1")).records.length, 1);
  });
});

describe("SOAP calls", () => {
  test("soap.convertLead() makes an account, a contact and an opportunity", async () => {
    const [lead, lead2] = await create(conn, track, "Lead", [
      { LastName: tagged("convert"), Company: tagged("Convert Co"), Email: "convert@example.com" },
      { LastName: tagged("convert 2"), Company: tagged("Convert Co") },
    ]);
    const ret = await conn.soap.convertLead({ leadId: lead, convertedStatus: "Closed - Converted" });
    assert.equal(ret.success, true);
    track("Opportunity", [ret.opportunityId]);
    track("Contact", [ret.contactId]);
    track("Account", [ret.accountId]);
    const l = await conn.sobject("Lead").retrieve(lead);
    assert.equal(l.IsConverted, true);
    assert.equal(l.ConvertedContactId, ret.contactId);
    assert.equal((await conn.sobject("Contact").retrieve(ret.contactId)).Email, "convert@example.com");

    const into = await conn.soap.convertLead({
      leadId: lead2,
      accountId: ret.accountId,
      convertedStatus: "Closed - Converted",
      doNotCreateOpportunity: true,
    });
    assert.equal(into.accountId, ret.accountId);
    assert.equal(into.opportunityId, null);
    track("Contact", [into.contactId]);
    await rejects(conn.sobject("Lead").update({ Id: lead, Company: "x" }), "CANNOT_UPDATE_CONVERTED_LEAD");
  });

  test("soap.merge() keeps the master and deletes the others", async () => {
    const ids = await create(conn, track, "Lead", [1, 2, 3].map((i) => ({ LastName: tagged(`merge ${i}`), Company: "Merge Co" })));
    const ret = await conn.soap.merge({ masterRecord: { type: "Lead", Id: ids[0], Title: "Kept" }, recordToMergeIds: ids.slice(1) });
    assert.equal(ret.success, true);
    assert.equal(ret.id, ids[0]);
    assert.equal(ret.mergedRecordIds.length, 2);
    assert.equal((await conn.sobject("Lead").retrieve(ids[0])).Title, "Kept");
    const left = await conn.query(`SELECT Id FROM Lead WHERE Id IN ('${ids[1]}', '${ids[2]}')`);
    assert.equal(left.totalSize, 0);
  });

  test("soap.undelete() restores deleted records, and fails for others", async () => {
    const [a, b] = await create(conn, track, "Contact", [{ LastName: tagged("undelete 1") }, { LastName: tagged("undelete 2") }]);
    await conn.sobject("Contact").destroy(a);
    const res = await conn.soap.undelete([a, b]);
    assert.equal(res[0].success, true);
    assert.equal(res[1].success, false);
    assert.ok(res[1].errors.length > 0);
    assert.equal((await conn.sobject("Contact").retrieve(a)).Id, a);
  });
});

describe("UI metadata", () => {
  test("layouts(), compactLayouts() and approvalLayouts()", async () => {
    const Account = conn.sobject("Account");
    const layouts = await Account.layouts();
    assert.ok(layouts.layouts.length > 0);
    assert.ok(Array.isArray(layouts.layouts[0].detailLayoutSections));
    assert.ok(Array.isArray(layouts.recordTypeMappings));
    const compact = await Account.compactLayouts();
    assert.ok(compact.compactLayouts.some((l) => l.objectType === "Account"));
    assert.ok(Array.isArray((await Account.approvalLayouts()).approvalLayouts));
    assert.ok((await conn.sobject("User").layouts("UserAlt")).layouts.length > 0);
  });

  test("listviews(), and a list view's describe, results and explain", async () => {
    const Account = conn.sobject("Account");
    const { listviews } = await Account.listviews();
    const all = listviews.find((v) => v.developerName === "AllAccounts");
    assert.ok(all, JSON.stringify(listviews));
    const view = Account.listview(all.id);
    const d = await view.describe();
    assert.equal(d.sobjectType, "Account");
    assert.match(d.query, /FROM Account/);
    const r = await view.results();
    assert.equal(typeof r.size, "number");
    assert.ok(Array.isArray(r.records));
    const plan = await view.explain();
    assert.ok(plan.plans.length > 0);
  });

  test("query().explain() returns the plans", async () => {
    const plan = await conn.query(`SELECT Id FROM Account WHERE Name = '${TAG}'`).explain();
    assert.ok(plan.plans.some((p) => p.leadingOperationType === "TableScan"));
  });

  test("tabs() and theme()", async () => {
    const tabs = await conn.tabs();
    assert.ok(tabs.some((t) => t.sobjectName === "Account" && t.name === "standard-Account"));
    const theme = await conn.theme();
    assert.ok(theme.themeItems.some((t) => t.name === "Account"));
  });
});

describe("browser and server-to-server logins", () => {
  const localOnly = IS_LOCALFORCE ? {} : { skip: "needs a browser to log in, or a connected app with a certificate" };
  const redirectUri = "http://localhost:1717/OauthRedirect";

  test("the authorization page lists the users to sign in as", localOnly, async () => {
    const oauth2 = new jsforce.OAuth2({ loginUrl: process.env.SF_LOGIN_URL, clientId: "app", redirectUri });
    const res = await fetch(oauth2.getAuthorizationUrl({ state: "s1" }));
    assert.equal(res.status, 200);
    const page = await res.text();
    assert.ok(page.includes(process.env.SF_USERNAME), "the session user is on the picker");
  });

  test("authorization code with PKCE, then a refresh token", localOnly, async () => {
    const oauth2 = new jsforce.OAuth2({ loginUrl: process.env.SF_LOGIN_URL, clientId: "app", clientSecret: "secret", redirectUri, useVerifier: true });
    const url = oauth2.getAuthorizationUrl({ state: "xyz", login_hint: process.env.SF_USERNAME });
    assert.match(url, /code_challenge=/);
    const res = await fetch(url, { redirect: "manual" });
    assert.ok([302, 303].includes(res.status), `redirected (${res.status})`);
    const back = new URL(res.headers.get("location"));
    assert.equal(back.origin + back.pathname, redirectUri);
    assert.equal(back.searchParams.get("state"), "xyz");

    const c = new jsforce.Connection({ oauth2, version: API_VERSION });
    const info = await c.authorize(back.searchParams.get("code"));
    assert.match(info.id, /^005/);
    assert.ok(c.refreshToken, "a refresh token comes with the code flow");
    assert.equal((await c.query(`SELECT Id FROM User WHERE Id = '${info.id}'`)).totalSize, 1);
    // A code works once.
    await rejects(new jsforce.Connection({ oauth2, version: API_VERSION }).authorize(back.searchParams.get("code")), "invalid_grant");

    const refreshed = await oauth2.refreshToken(c.refreshToken);
    assert.notEqual(refreshed.access_token, c.accessToken);
    assert.ok(refreshed.id.endsWith(`/${info.id}`), "the refreshed session is the same user's");
    const r = new jsforce.Connection({ instanceUrl: refreshed.instance_url, accessToken: refreshed.access_token, version: API_VERSION });
    assert.equal((await r.query("SELECT Id FROM User LIMIT 1")).records.length, 1);
  });

  test("the JWT bearer flow", localOnly, async () => {
    const { generateKeyPairSync, createSign } = await import("node:crypto");
    const { privateKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
    const b64 = (v) => Buffer.from(typeof v === "string" ? v : JSON.stringify(v)).toString("base64url");
    const assertion = (sub) => {
      const body = `${b64({ alg: "RS256" })}.${b64({ iss: "app", sub, aud: "https://login.salesforce.com", exp: Math.floor(Date.now() / 1000) + 180 })}`;
      return `${body}.${createSign("RSA-SHA256").update(body).sign(privateKey).toString("base64url")}`;
    };
    const oauth2 = new jsforce.OAuth2({ loginUrl: process.env.SF_LOGIN_URL, clientId: "app" });
    const grant_type = "urn:ietf:params:oauth:grant-type:jwt-bearer";
    const c = new jsforce.Connection({ oauth2, version: API_VERSION });
    const info = await c.authorize({ grant_type, assertion: assertion(process.env.SF_USERNAME) });
    assert.match(info.id, /^005/);
    assert.equal(c.refreshToken, undefined, "no refresh token with a JWT");
    await rejects(new jsforce.Connection({ oauth2, version: API_VERSION }).authorize({ grant_type, assertion: assertion("nobody-at-all@example.invalid") }), "invalid_grant");
  });
});

describe("limitInfo", () => {
  test("jsforce reads Sforce-Limit-Info, and usage goes up with each call", async () => {
    await conn.query("SELECT Id FROM User LIMIT 1");
    const before = conn.limitInfo.apiUsage.used;
    assert.ok(before > 0);
    assert.ok(conn.limitInfo.apiUsage.limit >= before);
    await conn.query("SELECT Id FROM User LIMIT 1");
    assert.ok(conn.limitInfo.apiUsage.used > before);
  });

  test("limits()", async () => {
    const limits = await conn.limits();
    assert.ok(limits.DailyApiRequests.Max > 0);
  });
});

describe("paging", () => {
  const COUNT = 250;
  const soql = `SELECT Id, LastName FROM Contact WHERE ${likeTag("LastName")} AND LastName LIKE '%paging%' ORDER BY LastName`;
  const batch200 = { headers: { "Sforce-Query-Options": "batchSize=200" } };
  let ids;

  before(async () => {
    ids = await create(conn, track, "Contact", Array.from({ length: COUNT }, (_, i) => ({ LastName: `${TAG} paging ${String(i).padStart(4, "0")}` })));
  });

  test("queryMore fetches the next page", async () => {
    const first = await conn.query(soql, batch200);
    assert.equal(first.done, false);
    assert.equal(first.records.length, 200);
    const second = await conn.queryMore(first.nextRecordsUrl);
    assert.equal(second.done, true);
    assert.equal(second.records.length, COUNT - 200);
  });

  test("autoFetch follows nextRecordsUrl; maxFetch stops early", async () => {
    const all = await conn.query(soql, batch200).run({ autoFetch: true, maxFetch: 10_000 });
    assert.deepEqual(new Set(all.records.map((r) => r.Id)), new Set(ids));
    const some = await conn.query(soql, batch200).run({ autoFetch: true, maxFetch: 200 });
    assert.equal(some.records.length, 200);
  });

  test("record events stream every record", async () => {
    const seen = await new Promise((resolve, reject) => {
      let n = 0;
      conn
        .query(soql, batch200)
        .on("record", () => n++)
        .on("end", () => resolve(n))
        .on("error", reject)
        .run({ autoFetch: true, maxFetch: 10_000 });
    });
    assert.equal(seen, COUNT);
  });

  test("scanAll includes deleted records", async () => {
    await conn.sobject("Contact").destroy(ids.slice(0, 3));
    const live = await conn.query(soql);
    assert.equal(live.totalSize, COUNT - 3);
    const all = await conn.query(soql, { ...batch200, scanAll: true }).run({ autoFetch: true, maxFetch: 10_000, scanAll: true });
    assert.equal(all.records.length, COUNT);
  });
});

describe("query builder", () => {
  before(async () => {
    await create(conn, track, "Account", [
      { Name: tagged("qb Alpha"), Industry: "Technology", Tier__c: "Gold", NumberOfEmployees: 50 },
      { Name: tagged("qb beta"), Industry: "Retail", Tier__c: "Silver", NumberOfEmployees: 10 },
      { Name: tagged("qb Gamma"), NumberOfEmployees: 500 },
      { Name: tagged("qb delta"), Industry: "Technology", Tier__c: "Bronze" },
    ]);
  });
  const mine = { $like: `${TAG} qb%` };

  test("find() with conditions, sort and limit", async () => {
    const recs = await conn.sobject("Account").find({ Name: mine, Industry: "Technology" }, ["Name", "Tier__c"]).sort({ Name: -1 }).limit(10);
    assert.deepEqual(pluck(recs, "Tier__c"), ["Bronze", "Gold"]);
  });

  test("find() with $in and $gte", async () => {
    const recs = await conn.sobject("Account").find({ Name: mine, Tier__c: { $in: ["Gold", "Silver"] }, NumberOfEmployees: { $gte: 10 } }, ["Name"]).sort("Name");
    assert.deepEqual(pluck(recs), [tagged("qb Alpha"), tagged("qb beta")]);
  });

  test("count() and findOne()", async () => {
    assert.equal(await conn.sobject("Account").count({ Name: mine }), 4);
    assert.equal(await conn.sobject("Account").count({ Name: mine, Industry: null }), 1);
    const r = await conn.sobject("Account").findOne({ Name: tagged("qb Gamma") }, ["Id", "NumberOfEmployees"]);
    assert.equal(r.NumberOfEmployees, 500);
  });

  test("select() with a child subquery through include()", async () => {
    const recs = await conn
      .sobject("Account")
      .find({ Name: tagged("qb Gamma") }, ["Name"])
      .include("Contacts")
      .select("LastName")
      .end();
    assert.equal(recs.length, 1);
    assert.equal(recs[0].Contacts, null);
  });
});

describe("describe", () => {
  test("describe$ caches, and agrees with describe", async () => {
    const a = await conn.describe$("Project__c");
    const b = await conn.describe("Project__c");
    assert.deepEqual(a.fields.map((f) => f.name), b.fields.map((f) => f.name));
  });

  test("key prefixes in the global describe are unique", async () => {
    const prefixes = (await conn.describeGlobal()).sobjects.map((s) => s.keyPrefix).filter(Boolean);
    assert.equal(new Set(prefixes).size, prefixes.length);
  });
});

describe("collections", () => {
  test("allowRecursive splits large batches into 200s", async () => {
    const res = await conn.sobject("Account").create(Array.from({ length: 450 }, (_, i) => ({ Name: tagged(`recursive ${i}`) })), { allowRecursive: true, allOrNone: true });
    track("Account", res.map((r) => r.id));
    assert.equal(res.length, 450);
    assert.ok(res.every((r) => r.success));
  });

  test("without allowRecursive, jsforce refuses more than 200 records", async () => {
    await assert.rejects(conn.sobject("Account").create(Array.from({ length: 201 }, (_, i) => ({ Name: tagged(`x ${i}`) }))));
  });

  test("the API refuses more than 200 records in one call", async () => {
    const res = await raw(conn, "POST", "/composite/sobjects", {
      body: { records: Array.from({ length: 201 }, (_, i) => ({ attributes: { type: "Account" }, Name: tagged(`too many ${i}`) })) },
    });
    assert.equal(res.status, 400);
    assert.equal((await conn.query(`SELECT COUNT() FROM Account WHERE Name LIKE '${TAG} too many%'`)).totalSize, 0);
  });
});

describe("search, replication, recent items and the Tooling API", () => {
  test("search() runs SOSL", async () => {
    const [id] = await create(conn, track, "Account", [{ Name: tagged("jsforce searchable") }]);
    if (!IS_LOCALFORCE) await new Promise((r) => setTimeout(r, 20_000)); // a real org indexes new records after a delay
    const res = await conn.search(`FIND {${TAG} jsforce} IN NAME FIELDS RETURNING Account(Id, Name)`);
    assert.deepEqual(pluck(res.searchRecords, "Id"), [id]);
  });

  test("sobject().updated() and deleted() for replication", async () => {
    const start = new Date(Date.now() - 60_000);
    const [id] = await create(conn, track, "Account", [{ Name: tagged("jsforce replicated") }]);
    const end = new Date(Date.now() + 60_000);
    const updated = await conn.sobject("Account").updated(start, end);
    assert.ok(updated.ids.includes(id));
    await conn.sobject("Account").destroy(id);
    const deleted = await conn.sobject("Account").deleted(start, end);
    assert.ok(deleted.deletedRecords.some((r) => r.id === id));
  });

  test("recent() lists records viewed with FOR VIEW", async () => {
    const [id] = await create(conn, track, "Account", [{ Name: tagged("jsforce viewed") }]);
    await conn.query(`SELECT Id FROM Account WHERE Id = '${id}' FOR VIEW`);
    const recent = await conn.recent(200);
    assert.ok(recent.some((r) => r.Id === id));
    const forType = await conn.sobject("Account").recent();
    assert.ok(forType.some((r) => r.Id === id));
  });

  test("tooling.query() and tooling describes", async () => {
    const classes = await conn.tooling.query("SELECT Id, Name FROM ApexClass LIMIT 5");
    assert.ok(Array.isArray(classes.records));
    const rules = await conn.tooling.query("SELECT ValidationName, Active FROM ValidationRule WHERE ValidationName = 'Budget_Not_Negative'");
    assert.deepEqual(pluck(rules.records, "ValidationName"), ["Budget_Not_Negative"]);
    const global = await conn.tooling.describeGlobal();
    assert.ok(global.sobjects.some((s) => s.name === "ApexClass"));
    const apexClass = await conn.tooling.sobject("ApexClass").describe();
    assert.equal(apexClass.name, "ApexClass");
  });

  test("formula fields come back from the query builder", async () => {
    const [id] = await create(conn, track, "Project__c", [{ Name: tagged("jsforce formula"), Budget__c: 4200 }]);
    const r = await conn.sobject("Project__c").findOne({ Id: id }, ["Budget_K__c", "Is_Big__c"]);
    assert.equal(r.Budget_K__c, 4.2);
    assert.equal(r.Is_Big__c, false);
  });
});
