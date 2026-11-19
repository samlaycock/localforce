// The SDK-independent half of the conformance runner: loading the spec,
// placeholders, and matching expected values. tests/python and tests/go
// implement the same rules; see tests/conformance/README.md.

import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const SPEC_DIR = resolve(dirname(fileURLToPath(import.meta.url)), "../../conformance/cases");

/** Every spec file, in name order: `{ file, about, setup, cases }`. */
export function loadSpec() {
  return readdirSync(SPEC_DIR)
    .filter((f) => f.endsWith(".json"))
    .sort()
    .map((f) => ({ file: f.replace(/\.json$/, ""), setup: [], ...JSON.parse(readFileSync(join(SPEC_DIR, f), "utf8")) }));
}

/** `lf` and 12 lowercase letters and digits: always 14 characters. */
export const newTag = () => `lf${Date.now().toString(36).slice(-6).padStart(6, "0")}${randomBytes(3).toString("hex")}`;

const PLACEHOLDER = /\{([a-zA-Z_][a-zA-Z0-9_]*)(?::([-+]?\d+))?\}/g;
const WHOLE = /^\{([a-zA-Z_][a-zA-Z0-9_]*)\}$/;

function utcDate(days) {
  const d = new Date();
  d.setUTCDate(d.getUTCDate() + days);
  return d.toISOString().slice(0, 10);
}

/**
 * Replace placeholders in every string in `value`. `ctx` has `tag`, `user`,
 * `user2`, `version` and `saved` (a Map of names to values).
 */
export function substitute(value, ctx) {
  if (typeof value === "string") {
    const whole = value.match(WHOLE);
    if (whole && ctx.saved.has(whole[1])) return ctx.saved.get(whole[1]);
    return value.replace(PLACEHOLDER, (m, name, arg) => {
      switch (name) {
        case "tag":
          return ctx.tag;
        case "date":
          return utcDate(Number(arg ?? 0));
        case "year":
          return utcDate(Number(arg ?? 0)).slice(0, 4);
        case "now":
          return new Date().toISOString();
        case "user":
          return ctx.user;
        case "version":
          return ctx.version;
        case "user2":
          if (!ctx.user2) throw new Error("{user2} needs a second user: add \"needs\": [\"user2\"] to the case");
          return ctx.user2;
      }
      if (ctx.saved.has(name)) {
        const v = String(ctx.saved.get(name));
        return arg === "15" ? v.slice(0, 15) : v;
      }
      // Names the step itself saves are filled in when its expectations are checked.
      if (ctx.later?.has(name)) return m;
      throw new Error(`unknown placeholder ${m}`);
    });
  }
  if (Array.isArray(value)) return value.map((v) => substitute(v, ctx));
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, substitute(v, ctx)]));
  return value;
}

/** Follow a dotted path ("Account.Name", "1.body.id") into a value. */
export function dig(value, path) {
  return path.split(".").reduce((v, k) => (v == null ? undefined : v[k]), value);
}

const show = (v) => (v === undefined ? "undefined" : JSON.stringify(v));
const fail = (where, msg) => {
  throw new assert.AssertionError({ message: `${where || "value"}: ${msg}` });
};
const SF_DATETIME = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}\+0000$/;
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/**
 * Assert that `actual` matches `expected` (see "Matching" in the spec's
 * README). `exact` makes objects need the same keys (apart from attributes),
 * as query records do.
 */
export function match(expected, actual, where = "", exact = false) {
  if (typeof expected === "string" && expected.startsWith("$")) {
    switch (expected) {
      case "$id":
        if (typeof actual !== "string" || !/^[a-zA-Z0-9]{18}$/.test(actual)) fail(where, `expected an 18-character id, got ${show(actual)}`);
        return;
      case "$datetime":
        if (typeof actual !== "string" || !SF_DATETIME.test(actual)) fail(where, `expected a Salesforce datetime, got ${show(actual)}`);
        return;
      case "$recent":
        if (typeof actual !== "string" || !SF_DATETIME.test(actual)) fail(where, `expected a Salesforce datetime, got ${show(actual)}`);
        if (Math.abs(Date.parse(actual.replace("+0000", "Z")) - Date.now()) > 10 * 60_000) fail(where, `expected a time within 10 minutes of now, got ${actual}`);
        return;
      case "$any":
        if (actual === undefined) fail(where, "expected a value");
        return;
      case "$notnull":
        if (actual == null) fail(where, `expected a value other than null, got ${show(actual)}`);
        return;
      case "$absent":
        if (actual !== undefined) fail(where, `expected no value, got ${show(actual)}`);
        return;
    }
  }
  if (isObject(expected)) {
    const keys = Object.keys(expected);
    if (keys.length === 1 && keys[0].startsWith("$")) return matcher(keys[0], expected[keys[0]], actual, where, exact);
    if (!isObject(actual)) fail(where, `expected an object, got ${show(actual)}`);
    for (const k of keys) {
      const sub = where ? `${where}.${k}` : k;
      if (expected[k] === "$absent") {
        if (k in actual) fail(sub, `expected no such key, got ${show(actual[k])}`);
        continue;
      }
      if (!(k in actual)) fail(sub, `missing (have ${Object.keys(actual).join(", ")})`);
      // attributes are only ever checked as a subset.
      match(expected[k], actual[k], sub, exact && k !== "attributes");
    }
    if (exact) {
      const extra = Object.keys(actual).filter((k) => k !== "attributes" && !(k in expected));
      if (extra.length) fail(where, `unexpected keys ${extra.join(", ")}`);
    }
    return;
  }
  if (Array.isArray(expected)) {
    // A child relationship in a query record: { totalSize, done, records }.
    const list = isObject(actual) && Array.isArray(actual.records) ? actual.records : actual;
    if (!Array.isArray(list)) fail(where, `expected an array, got ${show(actual)}`);
    if (list.length !== expected.length) fail(where, `expected ${expected.length} items, got ${list.length}: ${show(list)}`);
    expected.forEach((e, i) => match(e, list[i], `${where}[${i}]`, exact));
    return;
  }
  if (typeof expected === "number") {
    if (typeof actual !== "number" || Math.abs(expected - actual) > 1e-9) fail(where, `expected ${expected}, got ${show(actual)}`);
    return;
  }
  if (expected !== actual) fail(where, `expected ${show(expected)}, got ${show(actual)}`);
}

function matcher(name, arg, actual, where, exact) {
  switch (name) {
    case "$regex":
      if (typeof actual !== "string" || !new RegExp(arg).test(actual)) fail(where, `expected a match for /${arg}/, got ${show(actual)}`);
      return;
    case "$approx":
      if (typeof actual !== "number" || Math.abs(actual - arg) > 0.01) fail(where, `expected about ${arg}, got ${show(actual)}`);
      return;
    case "$gt":
      if (actual == null || !(actual > arg)) fail(where, `expected more than ${show(arg)}, got ${show(actual)}`);
      return;
    case "$lt":
      if (actual == null || !(actual < arg)) fail(where, `expected less than ${show(arg)}, got ${show(actual)}`);
      return;
    case "$length": {
      const list = isObject(actual) && Array.isArray(actual.records) ? actual.records : actual;
      if (!Array.isArray(list) || list.length !== arg) fail(where, `expected ${arg} items, got ${show(actual)}`);
      return;
    }
    case "$contains":
    case "$unordered": {
      const list = isObject(actual) && Array.isArray(actual.records) ? actual.records : actual;
      if (!Array.isArray(list)) fail(where, `expected an array, got ${show(actual)}`);
      if (name === "$unordered" && list.length !== arg.length) fail(where, `expected ${arg.length} items, got ${list.length}: ${show(list)}`);
      const used = new Set();
      for (const e of arg) {
        const i = list.findIndex((a, j) => !used.has(j) && matches(e, a, exact));
        if (i < 0) fail(where, `no item matches ${show(e)} in ${show(list)}`);
        used.add(i);
      }
      return;
    }
    case "$excludes": {
      const list = isObject(actual) && Array.isArray(actual.records) ? actual.records : actual;
      if (!Array.isArray(list)) fail(where, `expected an array, got ${show(actual)}`);
      for (const e of arg) {
        if (list.some((a) => matches(e, a, exact))) fail(where, `expected no item matching ${show(e)} in ${show(list)}`);
      }
      return;
    }
  }
  fail(where, `unknown matcher ${name}`);
}

export function matches(expected, actual, exact = false) {
  try {
    match(expected, actual, "", exact);
    return true;
  } catch (e) {
    if (e instanceof assert.AssertionError) return false;
    throw e;
  }
}

/**
 * Check a normalised error `{ code, fields, message }` against a step's
 * `error`: a code, a list of codes, or `{ code, fields, message }`.
 */
export function matchError(expected, actual, where = "error") {
  const exp = typeof expected === "string" || Array.isArray(expected) ? { code: expected } : expected;
  const codes = [].concat(exp.code);
  if (!codes.includes(actual.code)) fail(where, `expected ${codes.join(" or ")}, got ${actual.code}: ${actual.message}`);
  if (exp.fields) {
    const got = [...(actual.fields || [])].sort();
    if (JSON.stringify(got) !== JSON.stringify([...exp.fields].sort())) fail(where, `expected fields ${show(exp.fields)}, got ${show(actual.fields)}`);
  }
  if (exp.message && !String(actual.message).includes(exp.message)) fail(where, `expected the message to contain ${show(exp.message)}, got ${show(actual.message)}`);
}

/**
 * Check save results (`{ success, id, created, errors }`, one per record)
 * against `results`: "ok", `{ created }` or `{ error, fields }`.
 */
export function matchResults(expected, actual) {
  if (!Array.isArray(actual) || actual.length !== expected.length) fail("results", `expected ${expected.length} results, got ${show(actual)}`);
  expected.forEach((e, i) => {
    const r = actual[i];
    const where = `results[${i}]`;
    if (e === "ok" || (isObject(e) && !("error" in e))) {
      if (!r.success) fail(where, `expected success, got ${show(r.errors)}`);
      if (isObject(e) && "created" in e && r.created !== e.created) fail(where, `expected created ${e.created}, got ${show(r.created)}`);
      return;
    }
    if (r.success) fail(where, `expected ${show(e.error)}, got success`);
    const err = r.errors[0] || {};
    matchError({ code: e.error, fields: e.fields, message: e.message }, { code: err.statusCode, fields: err.fields, message: err.message }, where);
  });
}
