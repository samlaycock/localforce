// Runs the shared conformance spec (tests/conformance) through jsforce.
//
// How each operation maps to jsforce:
// - create / retrieve / update / upsert / delete: conn.sobject(type).create(),
//   .retrieve(), .update(), .upsert() and .destroy().
// - query: conn.query(soql).run({ autoFetch }), which follows nextRecordsUrl;
//   queryAll is jsforce's `scanAll` option.
// - describe / describe_global: conn.describe() and conn.describeGlobal().
// - create_many / update_many / upsert_many / delete_many / retrieve_many: the
//   same sobject methods with arrays (jsforce's sObject Collections support).
//   Records of mixed types go through conn.requestPost("/composite/sobjects"),
//   since jsforce gives every record of a call the same type.
// - composite: conn.requestPost("/composite").
// - request: fetch() with the connection's token.

import { after, before, describe, test } from "node:test";
import { API_VERSION, connect, raw, userIds } from "./lib/sf.mjs";
import { dig, loadSpec, match, matchError, matchResults, newTag, substitute } from "./lib/spec.mjs";

const conn = await connect();
const users = await userIds(conn);
const V = `/services/data/v${API_VERSION}`;

/** A jsforce error as `{ code, fields, message }`. */
function normaliseError(err) {
  return { code: err.errorCode || err.name, fields: err.data?.fields || err.fields || [], message: err.message };
}

/** A save result as `{ success, id, created, errors: [{ statusCode, fields, message }] }`. */
const results = (res) => [].concat(res).map((r) => ({ ...r, errors: r.errors || [] }));

/** Run one operation; returns `{ value, results, created }` for the expectations. */
async function operate(step) {
  const many = (opts = {}) => ({ allOrNone: !!step.all_or_none, ...opts });
  if ("create" in step) {
    const r = await conn.sobject(step.create).create(step.record);
    if (!r.success) throw Object.assign(new Error(r.errors[0]?.message), { errorCode: r.errors[0]?.statusCode, fields: r.errors[0]?.fields });
    return { value: r, created: [r.id] };
  }
  if ("retrieve" in step) return { value: await conn.sobject(step.retrieve).retrieve(step.id, step.fields ? { fields: step.fields } : {}) };
  if ("update" in step) return { value: await conn.sobject(step.update).update({ ...step.record, Id: step.id }) };
  if ("upsert" in step) {
    const r = await conn.sobject(step.upsert).upsert({ ...step.record, [step.field]: step.value }, step.field);
    return { value: r, created: r.created ? [r.id] : [] };
  }
  if ("delete" in step) return { value: await conn.sobject(step.delete).destroy(step.id) };
  if ("query" in step) {
    const scanAll = !!step.all;
    const res = await conn.query(step.query, { scanAll }).run({ autoFetch: true, maxFetch: 1_000_000, scanAll });
    return { value: res };
  }
  if ("describe" in step) return { value: await conn.describe(step.describe) };
  if ("describe_global" in step) return { value: await conn.describeGlobal() };
  if ("create_many" in step) {
    const out = [];
    for (let i = 0; i < step.records.length; i += 200) {
      const chunk = step.records.slice(i, i + 200);
      const res =
        step.create_many === true
          ? await conn.requestPost(`${V}/composite/sobjects`, { allOrNone: !!step.all_or_none, records: chunk })
          : await conn.sobject(step.create_many).create(chunk, many());
      out.push(...results(res));
    }
    return { value: out, results: out, created: out.filter((r) => r.success).map((r) => r.id) };
  }
  if ("update_many" in step) {
    const res =
      step.update_many === true
        ? await conn.request({ method: "PATCH", url: `${V}/composite/sobjects`, body: JSON.stringify({ allOrNone: !!step.all_or_none, records: step.records }), headers: { "content-type": "application/json" } })
        : await conn.sobject(step.update_many).update(step.records, many());
    const out = results(res);
    return { value: out, results: out };
  }
  if ("upsert_many" in step) {
    const out = results(await conn.sobject(step.upsert_many).upsert(step.records, step.field, many()));
    return { value: out, results: out, created: out.filter((r) => r.success && r.created).map((r) => r.id) };
  }
  if ("delete_many" in step) {
    const type = step.delete_many === true ? "Account" : step.delete_many;
    const out = results(await conn.sobject(type).destroy(step.ids, many()));
    return { value: out, results: out };
  }
  if ("retrieve_many" in step) return { value: await conn.sobject(step.retrieve_many).retrieve(step.ids, step.fields ? { fields: step.fields } : {}) };
  if ("composite" in step) {
    const res = await conn.requestPost(`${V}/composite`, step.composite);
    const created = res.compositeResponse.filter((r) => r.httpStatusCode === 201 && r.body?.id).map((r) => r.body.id);
    return { value: res.compositeResponse, created };
  }
  if ("request" in step) {
    const headers = { ...(step.headers || {}) };
    if (step.auth === false) headers.authorization = "";
    const body = "raw_body" in step ? step.raw_body : step.body;
    const res = await raw(conn, step.request, step.path, { body, headers });
    const created = step.request === "POST" && res.status === 201 && res.body?.id ? [res.body.id] : [];
    return { value: res.body, status: res.status, headers: res.headers, created };
  }
  if ("sleep" in step) {
    await new Promise((r) => setTimeout(r, step.sleep));
    return { value: null };
  }
  if ("settle" in step) {
    // Only a real org needs time to settle (e.g. to index records for search).
    if (target !== "localforce") await new Promise((r) => setTimeout(r, step.settle));
    return { value: null };
  }
  throw new Error(`no operation in step ${JSON.stringify(step)}`);
}

/** Run one step: operate, track what it created, save values, check expectations. */
async function runStep(fullStep, ctx, track) {
  // A template's {i} is filled in per record, below.
  const { template, ...rawStep } = fullStep;
  const later = new Set([...Object.keys(rawStep.save || {}), ...[].concat(rawStep.as || [])]);
  const step = substitute(rawStep, { ...ctx, later });
  if (template) {
    step.records = Array.from({ length: step.count }, (_, i) =>
      substitute(template, { ...ctx, saved: new Map([...ctx.saved, ["i", String(i).padStart(4, "0")]]) }),
    );
  }
  let out;
  try {
    out = await operate(step);
  } catch (err) {
    if (err.code === "ERR_ASSERTION" || !step.error) throw err;
    matchError(step.error, normaliseError(err));
    return;
  }
  track(out.created || []);
  if (step.error && !("request" in step)) throw new Error(`expected ${JSON.stringify(step.error)}, but the call succeeded`);

  if (step.as) {
    const names = [].concat(step.as);
    const ids = "results" in out ? out.results.map((r) => r.id) : [out.value.id];
    names.forEach((n, i) => ctx.saved.set(n, ids[i]));
  }
  const subject = "query" in step ? out.value.records[0] : out.value;
  for (const [name, path] of Object.entries(step.save || {})) ctx.saved.set(name, dig(subject, path));
  // Expectations can refer to values saved by this step.
  const expect = substitute(rawStep, ctx);

  if ("request" in step) {
    if ("status" in expect) match(expect.status, out.status, "status");
    if (expect.error) {
      matchError(expect.error, normaliseError({ errorCode: out.value?.[0]?.errorCode, fields: out.value?.[0]?.fields, message: out.value?.[0]?.message }));
    }
    if ("response" in expect) match(expect.response, out.value, "body");
    for (const [name, re] of Object.entries(expect.response_headers || {})) {
      const v = out.headers.get(name);
      if (re === "$absent") match("$absent", v ?? undefined, `header ${name}`);
      else match({ $regex: re }, v, `header ${name}`);
    }
    return;
  }
  if ("record" in expect && "retrieve" in step) match(expect.record, out.value, "record");
  if ("created" in expect) match(expect.created, out.value.created, "created");
  if ("results" in expect) matchResults(expect.results, out.results);
  if ("query" in step) {
    const recs = out.value.records;
    if ("total" in expect) match(expect.total, out.value.totalSize, "totalSize");
    if ("records" in expect) match(expect.unordered ? { $unordered: expect.records } : expect.records, recs, "records", true);
    if ("pluck" in expect) {
      const values = recs.map((r) => dig(r, expect.pluck) ?? null);
      if ("values" in expect) match(expect.unordered ? { $unordered: expect.values } : expect.values, values, `values of ${expect.pluck}`);
      if ("contains" in expect) match({ $contains: expect.contains }, values, `values of ${expect.pluck}`);
      for (const v of expect.excludes || []) if (values.includes(v)) match("$absent", v, `values of ${expect.pluck} include ${v}`);
    }
  }
  if ("retrieve_many" in step && "records" in expect) match(expect.records, out.value, "records");
  if ("describe" in step || "describe_global" in step) {
    const d = out.value;
    if (expect.object) match(expect.object, d, "describe");
    for (const [name, exp] of Object.entries(expect.fields || {})) {
      match(exp, d.fields.find((f) => f.name === name), `field ${name}`);
    }
    for (const [name, exp] of Object.entries(expect.child_relationships || {})) {
      match(exp, d.childRelationships.find((c) => c.relationshipName === name), `child relationship ${name}`);
    }
    for (const [name, exp] of Object.entries(expect.sobjects || {})) {
      match(exp, d.sobjects.find((s) => s.name === name), `sobject ${name}`);
    }
  }
  if ("composite" in step && expect.responses) match(expect.responses, out.value, "compositeResponse");
}

/** Delete records newest first, ignoring failures (some are already gone). */
async function cleanUp(ids) {
  const list = [...ids].reverse();
  for (let i = 0; i < list.length; i += 200) {
    await raw(conn, "DELETE", `/composite/sobjects?allOrNone=false&ids=${list.slice(i, i + 200).join(",")}`).catch(() => {});
  }
}

const target = process.env.LF_TARGET || "localforce";

for (const spec of loadSpec()) {
  describe(spec.file, () => {
    const fileCtx = { tag: newTag(), user: users.user, user2: users.user2, version: API_VERSION, saved: new Map() };
    const setupIds = [];
    before(async () => {
      for (const step of spec.setup) await runStep(step, fileCtx, (ids) => setupIds.push(...ids));
    });
    after(() => cleanUp(setupIds));

    for (const c of spec.cases) {
      const opts = {};
      if (c.skip?.node) opts.skip = c.skip.node;
      else if ((c.needs || []).includes("user2") && !users.user2) opts.skip = "needs a second user (LF_SECOND_USERNAME)";
      else if (c.todo && target === "localforce") opts.todo = `not in localforce yet: ${c.todo}`;
      test(c.name, opts, async () => {
        const ctx = { ...fileCtx, saved: new Map(fileCtx.saved) };
        const ids = [];
        try {
          for (const [i, step] of c.steps.entries()) {
            try {
              await runStep(step, ctx, (created) => ids.push(...created));
            } catch (err) {
              err.message = `step ${i + 1} (${Object.keys(step)[0]}): ${err.message}`;
              throw err;
            }
          }
        } finally {
          await cleanUp(ids);
        }
      });
    }
  });
}
