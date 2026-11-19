// Helpers for the jsforce tests: connecting, unique names so tests never see
// each other's data (or a real org's), cleanup, and assertions for
// Salesforce errors.

import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { after } from "node:test";
import jsforce from "jsforce";

export const API_VERSION = "63.0";
export const TARGET = process.env.LF_TARGET || "localforce";
export const IS_LOCALFORCE = TARGET === "localforce";
export const DEV_API = process.env.LF_DEV_API;

/** Skip options for tests that only make sense on one target. */
export const localforceOnly = IS_LOCALFORCE ? {} : { skip: "localforce-only" };

/** A connection for the configured target and user. */
export async function connect(opts = {}) {
  const version = opts.version || API_VERSION;
  if (process.env.SF_ACCESS_TOKEN && !opts.username) {
    return new jsforce.Connection({ instanceUrl: process.env.SF_INSTANCE_URL, accessToken: process.env.SF_ACCESS_TOKEN, version });
  }
  const conn = new jsforce.Connection({
    oauth2: {
      loginUrl: process.env.SF_LOGIN_URL,
      clientId: process.env.SF_CLIENT_ID,
      clientSecret: process.env.SF_CLIENT_SECRET,
    },
    version,
  });
  await conn.login(opts.username || process.env.SF_USERNAME, opts.password || process.env.SF_PASSWORD);
  return conn;
}

/** The session user's Id, and a second active user's (or undefined). */
export async function userIds(conn) {
  const info = await conn.request("/services/oauth2/userinfo");
  const second = process.env.LF_SECOND_USERNAME;
  const res = second ? await conn.query(`SELECT Id FROM User WHERE Username = '${second}' AND IsActive = true`) : { records: [] };
  return { user: info.user_id, user2: res.records[0]?.Id };
}

/** Can this target log in with a username and password (rather than a token)? */
export const hasPassword = !!(process.env.SF_USERNAME && process.env.SF_PASSWORD);

/**
 * A tag unique to this spec file's run. Put it in names and filter queries
 * by it, so specs running in parallel (or an org's existing data) don't
 * interfere.
 */
export const TAG = `lf${Date.now().toString(36)}${randomBytes(3).toString("hex")}`;

export const tagged = (s = "") => `${TAG} ${s}`.trim();

/** `Name LIKE 'TAG%'` */
export const likeTag = (field = "Name") => `${field} LIKE '${TAG}%'`;

/**
 * Delete tracked records after the file's tests, children first (in the
 * reverse order they were tracked). Best effort: records may already be gone.
 */
export function cleanup(conn) {
  const tracked = [];
  after(async () => {
    for (const [type, ids] of tracked.reverse()) {
      for (let i = 0; i < ids.length; i += 200) {
        await conn.sobject(type).destroy(ids.slice(i, i + 200)).catch(() => {});
      }
    }
  });
  return (type, ...ids) => {
    tracked.push([type, ids.flat().filter(Boolean)]);
    return ids.length === 1 ? ids[0] : ids;
  };
}

/** Create records, assert they all succeeded, and track them for cleanup. */
export async function create(conn, track, type, records) {
  const many = Array.isArray(records);
  const list = many ? records : [records];
  const ids = [];
  for (let i = 0; i < list.length; i += 200) {
    const res = await conn.sobject(type).create(list.slice(i, i + 200), { allOrNone: true });
    for (const r of res) {
      assert.equal(r.success, true, `creating ${type}: ${JSON.stringify(r.errors)}`);
      ids.push(r.id);
    }
  }
  track(type, ids);
  return many ? ids : ids[0];
}

/** Run SOQL and return the records. */
export async function q(conn, soql) {
  return (await conn.query(soql)).records;
}

/** The value of a field across query results. */
export const pluck = (records, field = "Name") => records.map((r) => field.split(".").reduce((v, k) => (v == null ? v : v[k]), r));

/**
 * Assert that `promise` rejects with a Salesforce error code (and optionally
 * the fields it names, or text in its message). Returns the error.
 */
export async function rejects(promise, code, { fields, message } = {}) {
  let err;
  try {
    await promise;
  } catch (e) {
    err = e;
  }
  assert.ok(err, `expected ${code}, but the call succeeded`);
  const codes = [].concat(code);
  const actual = err.errorCode || err.name;
  assert.ok(codes.includes(actual), `expected ${codes.join(" or ")}, got ${actual}: ${err.message}`);
  if (fields) assert.deepEqual([...(err.data?.fields || err.fields || [])].sort(), [...fields].sort(), "error fields");
  if (message) assert.match(err.message, message instanceof RegExp ? message : new RegExp(escapeRegExp(message)));
  return err;
}

/** Assert a failed save result (from collections) with a status code. */
export function failedWith(result, code, fields) {
  assert.equal(result.success, false, `expected ${code}, got success`);
  const e = result.errors[0];
  assert.equal(e.statusCode, code, e.message);
  if (fields) assert.deepEqual([...e.fields].sort(), [...fields].sort());
  return e;
}

function escapeRegExp(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** A UTC date `days` from today, as YYYY-MM-DD. */
export function day(days = 0) {
  const d = new Date();
  d.setUTCDate(d.getUTCDate() + days);
  return d.toISOString().slice(0, 10);
}

/** Salesforce's datetime format: 2026-01-31T09:30:00.000+0000 */
export const SF_DATETIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}\+0000$/;
export const ID18 = /^[a-zA-Z0-9]{18}$/;

/** Call localforce's own (non-Salesforce) API. */
export async function devApi(method, path, body) {
  const res = await fetch(DEV_API + path, {
    method,
    headers: body ? { "content-type": "application/json" } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  return { status: res.status, body: text ? JSON.parse(text) : null };
}

/** A raw REST call (relative to /services/data/vXX.X) that returns status and body instead of throwing. */
export async function raw(conn, method, path, { body, headers = {} } = {}) {
  const url = path.startsWith("/services") ? conn.instanceUrl + path : `${conn.instanceUrl}/services/data/v${conn.version}${path}`;
  const res = await fetch(url, {
    method,
    headers: { authorization: `Bearer ${conn.accessToken}`, ...(body !== undefined ? { "content-type": "application/json" } : {}), ...headers },
    body: body === undefined ? undefined : typeof body === "string" ? body : JSON.stringify(body),
  });
  const text = await res.text();
  let parsed = text;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch (_) {
    // not JSON
  }
  return { status: res.status, headers: res.headers, body: parsed };
}

export { assert };
