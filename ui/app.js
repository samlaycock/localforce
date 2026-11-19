"use strict";

// localforce web UI. No framework and no build step: the page is embedded in
// the binary. Records are read and written through the real REST API, so the
// page sees exactly the validation and errors an app would.

const API = "/__localforce/api";
const V = "62.0";

const S = {
  info: null,
  objects: [],
  byPrefix: {},
  token: null,
  userId: null,
  view: "data",
  // Data view
  object: null, // name
  schema: null, // /objects/{name}
  tab: "records",
  list: { offset: 0, limit: 50, order: "", dir: "asc", q: "", deleted: false, filter: null },
  page: null,
  selected: null, // record id, or "new"
  // Log view
  log: { entries: [], lastId: 0, showUi: false, selected: null, timer: null },
  // SOQL view
  query: { text: "", all: false, mode: "table", result: null, error: null },
};

/* ---------- helpers ---------- */

function el(tag, attrs, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k === "class") e.className = v;
    else if (k === "style") e.style.cssText = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else if (k === "value") e.value = v;
    else if (k === "checked" || k === "disabled" || k === "selected" || k === "multiple") e[k] = !!v;
    else e.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat(Infinity)) {
    if (c == null || c === false) continue;
    e.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return e;
}

const $ = (id) => document.getElementById(id);

/** Replace a node's children, flattening arrays and skipping nulls like `el`. */
function fill(node, ...children) {
  node.replaceChildren(...children.flat(Infinity).filter((c) => c != null && c !== false).map((c) => (c instanceof Node ? c : document.createTextNode(String(c)))));
  return node;
}

function store(key, value) {
  try {
    if (value === undefined) return localStorage.getItem("localforce." + key);
    if (value === null) localStorage.removeItem("localforce." + key);
    else localStorage.setItem("localforce." + key, value);
  } catch (_) {
    return null;
  }
}

function toast(msg, err) {
  const t = el("div", { class: "toast" + (err ? " err" : "") }, msg);
  $("toasts").append(t);
  setTimeout(() => t.remove(), err ? 6000 : 2800);
}

async function api(path, opts = {}) {
  const res = await fetch(API + path, {
    method: opts.method || "GET",
    headers: opts.body !== undefined ? { "content-type": "application/json" } : {},
    body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
  });
  const text = await res.text();
  const data = text ? JSON.parse(text) : null;
  if (!res.ok) {
    const msg = Array.isArray(data) ? data.map((e) => e.message).join("\n") : (data && data.error) || res.statusText;
    const e = new Error(msg);
    e.data = data;
    throw e;
  }
  return data;
}

async function newSession(userId) {
  const s = await api("/session", { method: "POST", body: { user: userId || null } });
  S.token = s.accessToken;
  S.userId = s.userId;
}

/** Call the Salesforce REST API as the chosen user. Returns { status, body }. */
async function rest(method, path, body, retried) {
  const res = await fetch(`/services/data/v${V}${path}`, {
    method,
    headers: {
      authorization: "Bearer " + S.token,
      "x-localforce-ui": "1",
      ...(body !== undefined ? { "content-type": "application/json" } : {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  if (res.status === 401 && !retried) {
    await newSession(S.userId);
    return rest(method, path, body, true);
  }
  const text = await res.text();
  return { status: res.status, body: text ? JSON.parse(text) : null };
}

function soqlString(s) {
  return "'" + String(s).replace(/\\/g, "\\\\").replace(/'/g, "\\'") + "'";
}

function apiNameFromLabel(label) {
  let base = label.trim().replace(/[^A-Za-z0-9]+/g, "_").replace(/_+/g, "_").replace(/^_|_$/g, "");
  if (!/^[A-Za-z]/.test(base)) base = "X" + base;
  return base.slice(0, 40).replace(/_$/, "") + "__c";
}

function pretty(text) {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch (_) {
    return text;
  }
}

function copy(text) {
  navigator.clipboard.writeText(text).then(() => toast("Copied"), () => toast("Couldn't copy", true));
}

function codeBlock(text) {
  return el("div", { class: "code-block" }, el("pre", { class: "mono" }, text), el("button", { class: "btn small", onclick: () => copy(text) }, "Copy"));
}

function download(name, text) {
  const a = el("a", { href: URL.createObjectURL(new Blob([text], { type: "text/plain" })), download: name });
  document.body.append(a);
  a.click();
  a.remove();
}

/** A modal dialog. `buttons` are elements; returns a function that closes it. */
function modal(title, body, buttons, opts = {}) {
  const close = () => {
    backdrop.remove();
    document.removeEventListener("keydown", onKey);
  };
  const onKey = (e) => e.key === "Escape" && close();
  const backdrop = el(
    "div",
    { class: "modal-backdrop", onmousedown: (e) => e.target === backdrop && close() },
    el(
      "div",
      { class: "modal" + (opts.wide ? " wide" : ""), role: "dialog" },
      el("div", { class: "modal-head" }, el("h2", {}, title), el("button", { class: "icon-btn", onclick: close, "aria-label": "Close" }, "✕")),
      el("div", { class: "modal-body" }, body),
      buttons && buttons.length ? el("div", { class: "modal-foot" }, buttons) : null,
    ),
  );
  document.addEventListener("keydown", onKey);
  document.body.append(backdrop);
  const first = backdrop.querySelector("input, select, textarea");
  if (first) first.focus();
  return close;
}

function confirmDialog(title, message, action, danger = true) {
  return new Promise((resolve) => {
    let close;
    const yes = el("button", { class: "btn " + (danger ? "danger" : "primary"), onclick: () => { close(); resolve(true); } }, action);
    const no = el("button", { class: "btn", onclick: () => { close(); resolve(false); } }, "Cancel");
    close = modal(title, el("p", {}, message), [no, yes]);
  });
}

/* ---------- value formatting ---------- */

const NUMERIC = ["int", "double", "currency", "percent"];

function fmtDateTime(v) {
  // Salesforce returns UTC: 2026-11-12T08:17:22.000+0000
  return v.slice(0, 10) + " " + v.slice(11, 19);
}

function fmtValue(field, v) {
  if (v == null || v === "") return el("span", { class: "null" }, "—");
  if (!field) return typeof v === "object" ? JSON.stringify(v) : String(v);
  switch (field.describeType) {
    case "boolean":
      return v ? "✓" : el("span", { class: "null" }, "✗");
    case "currency":
      return Number(v).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
    case "percent":
      return Number(v).toLocaleString() + "%";
    case "double":
    case "int":
      return Number(v).toLocaleString();
    case "datetime":
      return fmtDateTime(v);
    case "multipicklist":
      return String(v).split(";").join(", ");
    default:
      return String(v);
  }
}

function recordLink(id, label) {
  const obj = S.byPrefix[String(id).slice(0, 3)];
  if (!obj) return document.createTextNode(label || id);
  return el("a", { href: `#/data/${obj.name}/${id}`, onclick: (e) => e.stopPropagation() }, label || id);
}

/* ---------- objects ---------- */

async function refreshObjects() {
  S.objects = await api("/objects");
  S.byPrefix = Object.fromEntries(S.objects.map((o) => [o.keyPrefix, o]));
  renderSidebar();
}

function sourceBadge(source) {
  const labels = { standard: "Standard", config: "localforce.toml", metadata: "SFDX metadata", ui: "Created here" };
  return el("span", { class: "badge " + source, title: "Defined in: " + labels[source] }, labels[source]);
}

/* ---------- routing ---------- */

function parseHash() {
  const [path, qs] = location.hash.replace(/^#\/?/, "").split("?");
  const parts = path.split("/").filter(Boolean).map(decodeURIComponent);
  return { parts, params: new URLSearchParams(qs || "") };
}

async function route() {
  const { parts, params } = parseHash();
  const view = ["data", "query", "log"].includes(parts[0]) ? parts[0] : "data";
  S.view = view;
  for (const a of document.querySelectorAll(".views a")) a.classList.toggle("active", a.dataset.view === view);
  for (const v of ["data", "query", "log"]) $("view-" + v).hidden = v !== view;
  if (view === "log") startLog();
  else stopLog();
  if (view === "query") return renderQuery();
  if (view === "log") return renderLog();

  const name = parts[1] || store("object") || "Account";
  const obj = S.objects.find((o) => o.name.toLowerCase() === name.toLowerCase());
  if (!obj) {
    location.hash = "#/data/Account";
    return;
  }
  const changed = S.object !== obj.name;
  const before = { tab: S.tab, filter: JSON.stringify(S.list.filter) };
  if (changed) {
    S.object = obj.name;
    store("object", obj.name);
    S.tab = "records";
    S.list = { offset: 0, limit: 50, order: "", dir: "asc", q: "", deleted: false, filter: null };
  }
  const field = params.get("field");
  const value = params.get("value");
  const filter = field && value ? { field, value, label: params.get("label") || value } : null;
  if (JSON.stringify(filter) !== JSON.stringify(S.list.filter)) {
    S.list.filter = filter;
    S.list.offset = 0;
  }
  if (parts[2] === "fields" || parts[2] === "metadata") {
    S.tab = parts[2];
    S.selected = null;
  } else {
    S.tab = "records";
    S.selected = parts[2] || null;
  }
  // Selecting or closing a record keeps the table as it is (and its scroll).
  const sameList = !changed && before.tab === "records" && S.tab === "records" && before.filter === JSON.stringify(S.list.filter);
  if (sameList && $("records-body")) {
    if (S.selected) await openRecord(S.selected, true);
    else {
      $("view-data").querySelector(".drawer")?.remove();
      for (const tr of $("view-data").querySelectorAll("tr.selected")) tr.classList.remove("selected");
    }
    return;
  }
  await loadSchema();
  renderData();
}

function go(hash) {
  if (location.hash === hash) route();
  else location.hash = hash;
}

/* ---------- data view ---------- */

async function loadSchema() {
  S.schema = await api("/objects/" + encodeURIComponent(S.object));
}

function renderSidebar() {
  const root = $("view-data");
  let side = root.querySelector(".sidebar");
  if (!side) {
    side = el(
      "aside",
      { class: "sidebar" },
      el(
        "div",
        { class: "sidebar-head" },
        el("input", { type: "search", placeholder: "Filter objects", id: "object-filter", oninput: renderSidebar }),
        el("button", { class: "btn", title: "New custom object", onclick: newObjectDialog }, "+ New"),
      ),
      el("div", { class: "object-list", id: "object-list" }),
    );
    root.prepend(side);
  }
  const q = ($("object-filter").value || "").toLowerCase();
  const match = (o) => !q || o.name.toLowerCase().includes(q) || o.label.toLowerCase().includes(q);
  const item = (o) =>
    el(
      "a",
      { class: "object-item" + (o.name === S.object ? " active" : ""), href: "#/data/" + o.name, title: o.name },
      el("span", { class: "name" }, o.labelPlural),
      el("span", { class: "n" }, o.count.toLocaleString()),
    );
  const custom = S.objects.filter((o) => o.custom && match(o));
  const standard = S.objects.filter((o) => !o.custom && match(o));
  fill($("object-list"), 
    custom.length ? el("div", { class: "object-group" }, "Custom objects") : null,
    custom.map(item),
    standard.length ? el("div", { class: "object-group" }, "Standard objects") : null,
    standard.map(item),
    !custom.length && !standard.length ? el("div", { class: "empty" }, "No objects match") : null,
  );
}

function renderData() {
  renderSidebar();
  const root = $("view-data");
  root.querySelector(".pane")?.remove();
  root.querySelector(".drawer")?.remove();
  const s = S.schema;
  const tabs = [
    ["records", `Records (${s.count.toLocaleString()})`],
    ["fields", `Fields & relationships (${s.fields.length})`],
    ["metadata", "SFDX metadata"],
  ];
  const head = el(
    "div",
    { class: "pane-head" },
    el(
      "div",
      { class: "pane-title" },
      el("h1", {}, s.labelPlural),
      el("span", { class: "api" }, s.name),
      el("span", { class: "badge", title: "Key prefix: record ids start with this" }, s.keyPrefix),
      sourceBadge(s.source),
      el("div", { class: "spacer" }),
      s.editable ? el("button", { class: "btn small danger", onclick: deleteObject }, "Delete object") : null,
    ),
    s.description ? el("div", { class: "muted", style: "margin-top:4px" }, s.description) : null,
    el(
      "div",
      { class: "tabs" },
      tabs.map(([id, label]) =>
        el("button", { class: S.tab === id ? "active" : "", id: "tab-" + id, onclick: () => go(id === "records" ? `#/data/${s.name}` : `#/data/${s.name}/${id}`) }, label),
      ),
    ),
  );
  const pane = el("section", { class: "pane" }, head);
  root.append(pane);
  if (S.tab === "records") renderRecords(pane);
  else if (S.tab === "fields") renderFields(pane);
  else renderMetadata(pane);
}

/* ----- records table ----- */

function defaultColumns(s) {
  const nf = s.nameField;
  const skip = new Set(["Id", "IsDeleted", "SystemModstamp", "CreatedById", "LastModifiedById", "CreatedDate", "LastModifiedDate", "OwnerId", nf]);
  if (s.fields.some((f) => f.name === "Name" && f.kind === "compound")) {
    skip.add("FirstName");
    skip.add("LastName");
  }
  const cols = nf ? [nf] : [];
  const rest = s.fields.filter((f) => !skip.has(f.name) && f.describeType !== "textarea");
  rest.sort((a, b) => (b.required - a.required) || (b.custom - a.custom));
  cols.push(...rest.slice(0, 5).map((f) => f.name));
  cols.push("LastModifiedDate");
  return cols;
}

function columns() {
  const s = S.schema;
  const saved = store("cols." + s.name);
  if (saved) {
    const names = saved.split(",").filter((n) => s.fields.some((f) => f.name === n));
    if (names.length) return names;
  }
  return defaultColumns(s);
}

function fieldOf(name) {
  return S.schema.fields.find((f) => f.name === name);
}

/** The SOQL select path for a column: references also fetch the parent's name. */
function selectPaths(cols) {
  const paths = [];
  for (const c of cols) {
    paths.push(c);
    const f = fieldOf(c);
    if (f && f.relationshipName && f.referenceNameField) paths.push(`${f.relationshipName}.${f.referenceNameField}`);
  }
  return paths;
}

async function loadPage() {
  const L = S.list;
  const cols = columns();
  const params = new URLSearchParams({
    fields: selectPaths(cols).join(","),
    limit: L.limit,
    offset: L.offset,
    order: L.order,
    dir: L.dir,
    q: L.q,
    deleted: L.deleted ? "1" : "0",
  });
  if (L.filter) {
    params.set("field", L.filter.field);
    params.set("value", L.filter.value);
  }
  S.page = await api(`/records/${encodeURIComponent(S.object)}?${params}`);
}

function cellFor(rec, col) {
  const f = fieldOf(col);
  const v = rec[col];
  if (f && f.describeType === "reference" && v) {
    const parent = f.relationshipName && rec[f.relationshipName];
    const label = parent && f.referenceNameField ? parent[f.referenceNameField] : null;
    return recordLink(v, label || v);
  }
  if (f && f.nameField) return el("a", { href: `#/data/${S.object}/${rec.Id}` }, v == null ? rec.Id : String(v));
  return fmtValue(f, v);
}

async function renderRecords(pane) {
  const L = S.list;
  const cols = columns();
  const search = el("input", {
    type: "search",
    placeholder: "Search text fields or paste an Id",
    value: L.q,
    oninput: debounce(async (e) => {
      L.q = e.target.value;
      L.offset = 0;
      await refreshTable();
    }, 250),
  });
  const toolbar = el(
    "div",
    { class: "toolbar" },
    search,
    L.filter
      ? el("span", { class: "badge ui" }, `${L.filter.field} = ${L.filter.label} `, el("a", { href: `#/data/${S.object}`, title: "Clear filter" }, "✕"))
      : null,
    el("label", { class: "check" }, el("input", { type: "checkbox", checked: L.deleted, onchange: async (e) => { L.deleted = e.target.checked; L.offset = 0; await refreshTable(); } }), el("span", { id: "bin-label" }, `Recycle bin (${S.schema.deletedCount})`)),
    el("div", { class: "spacer" }),
    el("button", { class: "btn", onclick: columnsDialog }, "Columns"),
    el("button", { class: "btn primary", onclick: () => openRecord("new") }, "New " + S.schema.label),
  );
  const body = el("div", { class: "pane-body", id: "records-body" });
  const pager = el("div", { class: "pager", id: "pager" });
  pane.append(toolbar, body, pager);
  await refreshTable();
  if (S.selected) openRecord(S.selected, true);

  async function refreshTable() {
    // The table may have been replaced by another view since this was set up.
    if (!body.isConnected) return;
    try {
      await loadPage();
    } catch (e) {
      fill(body, el("div", { class: "alert err" }, el("pre", { class: "mono" }, e.message)));
      return;
    }
    const recs = S.page.records;
    const th = (c) => {
      const f = fieldOf(c);
      const sortPath = f && f.relationshipName && f.referenceNameField ? `${f.relationshipName}.${f.referenceNameField}` : c;
      const active = L.order === sortPath;
      return el(
        "th",
        {
          class: "sortable",
          title: c,
          onclick: async () => {
            L.dir = active && L.dir === "asc" ? "desc" : "asc";
            L.order = sortPath;
            await refreshTable();
          },
        },
        f ? f.label : c,
        active ? (L.dir === "asc" ? " ▲" : " ▼") : "",
      );
    };
    if (!recs.length) {
      fill(body, 
        el(
          "div",
          { class: "empty" },
          L.q || L.filter ? "No records match." : L.deleted ? "The recycle bin is empty." : `No ${S.schema.labelPlural.toLowerCase()} yet.`,
          !L.deleted && !L.q ? el("div", { style: "margin-top:10px" }, el("button", { class: "btn primary", onclick: () => openRecord("new") }, "New " + S.schema.label)) : null,
        ),
      );
    } else {
      fill(body, 
        el(
          "table",
          { class: "grid" },
          el("thead", {}, el("tr", {}, cols.map(th))),
          el(
            "tbody",
            {},
            recs.map((r) =>
              el(
                "tr",
                { class: "clickable" + (S.selected === r.Id ? " selected" : ""), "data-id": r.Id, onclick: () => go(`#/data/${S.object}/${r.Id}${filterQs()}`) },
                cols.map((c) => el("td", { class: NUMERIC.includes(fieldOf(c)?.describeType) ? "num" : "" }, cellFor(r, c))),
              ),
            ),
          ),
        ),
      );
    }
    const total = S.page.totalSize;
    const from = total ? L.offset + 1 : 0;
    const to = Math.min(L.offset + L.limit, total);
    fill(pager, 
      el("span", {}, `${from.toLocaleString()}–${to.toLocaleString()} of ${total.toLocaleString()}`),
      el("button", { class: "btn small", disabled: L.offset === 0, onclick: async () => { L.offset = Math.max(0, L.offset - L.limit); await refreshTable(); } }, "‹ Prev"),
      el("button", { class: "btn small", disabled: to >= total, onclick: async () => { L.offset += L.limit; await refreshTable(); } }, "Next ›"),
      el("div", { class: "spacer" }),
      el("button", { class: "btn small link", title: S.page.soql, onclick: () => { S.query.text = S.page.soql.replace(/ LIMIT \d+ OFFSET \d+$/, " LIMIT 200"); go("#/query"); } }, "Open in SOQL"),
    );
  }
  S.refreshTable = refreshTable;
}

function filterQs() {
  const f = S.list.filter;
  return f ? `?field=${encodeURIComponent(f.field)}&value=${encodeURIComponent(f.value)}&label=${encodeURIComponent(f.label)}` : "";
}

function columnsDialog() {
  const current = new Set(columns());
  const boxes = S.schema.fields.map((f) =>
    el("label", { class: "check" }, el("input", { type: "checkbox", value: f.name, checked: current.has(f.name) }), f.label, el("span", { class: "faint mono" }, " " + f.name)),
  );
  let close;
  const save = el("button", {
    class: "btn primary",
    onclick: () => {
      const chosen = boxes.map((b) => b.querySelector("input")).filter((i) => i.checked).map((i) => i.value);
      // Keep the current order for columns that stay, append new ones.
      const order = [...columns().filter((c) => chosen.includes(c)), ...chosen.filter((c) => !current.has(c))];
      store("cols." + S.schema.name, order.length ? order.join(",") : null);
      close();
      renderData();
    },
  }, "Apply");
  const reset = el("button", { class: "btn", onclick: () => { store("cols." + S.schema.name, null); close(); renderData(); } }, "Reset to default");
  close = modal("Columns", el("div", { class: "columns-list" }, boxes), [reset, save]);
}

/* ----- record drawer ----- */

function editableNow(f, isNew) {
  return isNew ? f.createable : f.updateable;
}

async function fetchRecord(id) {
  const s = S.schema;
  const paths = selectPaths(s.fields.map((f) => f.name));
  const res = await api("/query", { method: "POST", body: { soql: `SELECT ${paths.join(", ")} FROM ${s.name} WHERE Id = ${soqlString(id)}`, all: true } });
  return res.records[0] || null;
}

async function openRecord(id, fromRoute) {
  if (!fromRoute) {
    go(id === "new" ? `#/data/${S.object}/new${filterQs()}` : `#/data/${S.object}/${id}${filterQs()}`);
    return;
  }
  const root = $("view-data");
  root.querySelector(".drawer")?.remove();
  for (const tr of root.querySelectorAll("tr[data-id]")) tr.classList.toggle("selected", tr.dataset.id === id);
  const drawer = el("aside", { class: "drawer" }, el("div", { class: "empty" }, "Loading…"));
  root.append(drawer);
  const isNew = id === "new";
  let rec = null;
  if (!isNew) {
    try {
      rec = await fetchRecord(id);
    } catch (e) {
      fill(drawer, el("div", { class: "alert err" }, e.message));
      return;
    }
    // Another record (or none) was chosen while this one loaded.
    if (S.selected !== id || !drawer.isConnected) {
      drawer.remove();
      return;
    }
    if (!rec) {
      fill(drawer, el("div", { class: "drawer-head" }, el("h2", {}, "Not found")), el("div", { class: "drawer-body" }, el("p", { class: "muted" }, `No ${S.schema.label} with id ${id}.`), el("button", { class: "btn", onclick: closeDrawer }, "Close")));
      return;
    }
  }
  renderDrawer(drawer, rec, isNew);
}

function closeDrawer() {
  go(`#/data/${S.object}${filterQs()}`);
}

function renderDrawer(drawer, rec, isNew, draft = {}) {
  const s = S.schema;
  const deleted = rec && rec.IsDeleted;
  const controls = {}; // field name -> { get(): value, row }
  const initial = {};
  const errorBox = el("div", {});

  const prefill = { ...draft };
  if (isNew && S.list.filter) prefill[S.list.filter.field] = S.list.filter.value;

  const rows = { main: [], system: [], derived: [] };
  for (const f of s.fields) {
    const value = rec ? rec[f.name] : prefill[f.name] ?? (f.default ?? null);
    const canEdit = !deleted && editableNow(f, isNew);
    if (isNew && !canEdit) continue;
    const group = f.kind === "system" ? "system" : ["compound", "autonumber", "formula"].includes(f.kind) ? "derived" : "main";
    const row = fieldRow(f, value, canEdit, rec, controls);
    if (canEdit) initial[f.name] = controls[f.name].get();
    rows[group].push(row);
  }

  const save = el("button", { class: "btn primary", disabled: !isNew, onclick: doSave }, isNew ? "Create" : "Save");
  const markDirty = () => {
    if (isNew) return;
    const changed = Object.keys(controls).some((k) => JSON.stringify(controls[k].get()) !== JSON.stringify(initial[k]));
    save.disabled = !changed;
  };
  drawer.addEventListener("input", markDirty);
  drawer.addEventListener("change", markDirty);

  const title = isNew ? "New " + s.label : (s.nameField && rec[s.nameField]) || rec.Id;
  const actions = [];
  if (deleted) {
    actions.push(el("button", { class: "btn primary", onclick: doUndelete }, "Restore from recycle bin"));
  } else {
    actions.push(save);
    if (!isNew) {
      actions.push(el("button", { class: "btn", onclick: doClone }, "Clone"));
      actions.push(el("button", { class: "btn danger", onclick: doDelete }, "Delete"));
    }
  }
  actions.push(el("button", { class: "btn", onclick: closeDrawer }, isNew ? "Cancel" : "Close"));

  const related = el("div", { class: "related" });
  fill(drawer, 
    el(
      "div",
      { class: "drawer-head" },
      el("h2", {}, title, deleted ? el("span", { class: "badge err" }, "In recycle bin") : null),
      rec ? el("div", { class: "id", title: "Copy id", onclick: () => copy(rec.Id) }, rec.Id) : el("div", { class: "muted" }, "Fields marked * are required."),
      el("div", { class: "drawer-actions" }, actions),
    ),
    el(
      "div",
      { class: "drawer-body" },
      errorBox,
      rows.main,
      rows.derived.length ? [el("div", { class: "section-title" }, "Calculated"), rows.derived] : null,
      !isNew && s.childRelationships.length ? [el("div", { class: "section-title" }, "Related"), related] : null,
      rows.system.length ? [el("div", { class: "section-title" }, "System"), rows.system] : null,
    ),
  );
  if (!isNew && s.childRelationships.length) loadRelated(related, rec);

  async function doSave() {
    const body = {};
    for (const [k, c] of Object.entries(controls)) {
      const v = c.get();
      if (isNew ? v !== null && v !== "" : JSON.stringify(v) !== JSON.stringify(initial[k])) body[k] = v;
    }
    save.disabled = true;
    const res = isNew ? await rest("POST", `/sobjects/${s.name}`, body) : await rest("PATCH", `/sobjects/${s.name}/${rec.Id}`, body);
    if (res.status >= 400) {
      save.disabled = false;
      showErrors(res.body);
      return;
    }
    toast(isNew ? `${s.label} created` : `${s.label} saved`);
    await afterChange();
    // Only follow up if the user is still looking at this record.
    if (isNew && S.selected === "new") go(`#/data/${s.name}/${res.body.id}${filterQs()}`);
    else if (!isNew && S.selected === rec.Id) openRecord(rec.Id, true);
  }

  function showErrors(errors) {
    for (const c of Object.values(controls)) c.row.classList.remove("invalid");
    const list = Array.isArray(errors) ? errors : [{ message: JSON.stringify(errors) }];
    for (const e of list) for (const f of e.fields || []) controls[f]?.row.classList.add("invalid");
    fill(errorBox, 
      el("div", { class: "alert err" }, list.map((e) => el("div", {}, el("strong", { class: "mono" }, e.errorCode || e.statusCode || "ERROR"), " ", e.message))),
    );
    errorBox.scrollIntoView({ block: "nearest" });
  }

  async function doDelete() {
    const kids = s.childRelationships.filter((c) => c.cascadeDelete).map((c) => c.name);
    const msg = `Move this ${s.label.toLowerCase()} to the recycle bin?` + (kids.length ? ` Its ${kids.join(", ")} go with it.` : "");
    if (!(await confirmDialog("Delete " + s.label, msg, "Delete"))) return;
    const res = await rest("DELETE", `/sobjects/${s.name}/${rec.Id}`);
    if (res.status >= 400) return showErrors(res.body);
    toast(`${s.label} deleted`);
    await afterChange();
    closeDrawer();
  }

  async function doUndelete() {
    try {
      await api(`/records/${s.name}/${rec.Id}/undelete`, { method: "POST" });
    } catch (e) {
      return showErrors([{ message: e.message, errorCode: e.data && e.data.errorCode }]);
    }
    toast(`${s.label} restored`);
    await afterChange();
    if (S.selected === rec.Id) openRecord(rec.Id, true);
  }

  /** A new record starting from this one's values (except unique ones). */
  function doClone() {
    const copyOf = {};
    for (const f of s.fields) if (f.createable && rec[f.name] != null && !f.unique && !f.externalId) copyOf[f.name] = rec[f.name];
    history.replaceState(null, "", `#/data/${s.name}/new${filterQs()}`);
    S.selected = "new";
    renderDrawer(drawer, null, true, copyOf);
  }
}

async function afterChange() {
  await Promise.all([refreshObjects(), loadSchema()]);
  // Counts in the header and toolbar.
  const tab = $("tab-records");
  if (tab) tab.textContent = `Records (${S.schema.count.toLocaleString()})`;
  const bin = $("bin-label");
  if (bin) bin.textContent = `Recycle bin (${S.schema.deletedCount})`;
  if (S.refreshTable) await S.refreshTable();
}

async function loadRelated(box, rec) {
  let list;
  try {
    list = await api(`/records/${S.schema.name}/${rec.Id}/related`);
  } catch (e) {
    fill(box, el("div", { class: "muted" }, e.message));
    return;
  }
  const label = (S.schema.nameField && rec[S.schema.nameField]) || rec.Id;
  fill(box, 
    ...list.map((r) =>
      el(
        "a",
        { href: `#/data/${r.childObject}?field=${encodeURIComponent(r.field)}&value=${rec.Id}&label=${encodeURIComponent(label)}` },
        el("span", {}, r.relationship, el("span", { class: "faint" }, ` (${r.childObject}.${r.field})`)),
        el("span", { class: "muted" }, r.count.toLocaleString()),
      ),
    ),
  );
}

/** One label + control row. Registers a getter in `controls` for editable fields. */
function fieldRow(f, value, canEdit, rec, controls) {
  const label = el(
    "label",
    { title: f.description || f.name },
    f.label,
    canEdit && f.required && f.describeType !== "boolean" ? el("span", { class: "req" }, "*") : null,
    el("span", { class: "api" }, f.name),
  );
  const row = el("div", { class: "form-row" }, label);
  const control = el("div", { class: "control" });
  row.append(control);

  if (!canEdit) {
    let shown;
    if (f.describeType === "reference" && value) {
      const parent = rec && f.relationshipName && rec[f.relationshipName];
      shown = recordLink(value, parent && f.referenceNameField ? parent[f.referenceNameField] : value);
    } else if (f.kind === "formula") {
      shown = el("span", { title: f.formula }, fmtValue(f, value));
    } else {
      shown = fmtValue(f, value);
    }
    control.append(el("div", { class: "readonly-value" }, shown));
    return row;
  }

  let get;
  let set;
  const t = f.describeType;
  if (t === "boolean") {
    const input = el("input", { type: "checkbox", checked: !!value });
    control.append(el("label", { class: "check", style: "padding-top:6px" }, input));
    get = () => input.checked;
    set = (v) => (input.checked = !!v);
  } else if (t === "picklist") {
    const opts = [...f.values];
    if (value && !opts.includes(value)) opts.push(value);
    const select = el("select", {}, el("option", { value: "" }, "--None--"), opts.map((v) => el("option", { value: v, selected: v === value }, v)));
    control.append(select);
    get = () => select.value || null;
    set = (v) => (select.value = v || "");
  } else if (t === "multipicklist") {
    const chosen = new Set(value ? String(value).split(";") : []);
    const boxes = f.values.map((v) => el("label", { class: "check" }, el("input", { type: "checkbox", value: v, checked: chosen.has(v) }), v));
    control.append(el("div", { class: "multi" }, boxes));
    get = () => {
      const vals = boxes.map((b) => b.querySelector("input")).filter((i) => i.checked).map((i) => i.value);
      return vals.length ? vals.join(";") : null;
    };
    set = (v) => {
      const s = new Set(v ? String(v).split(";") : []);
      for (const b of boxes) b.querySelector("input").checked = s.has(b.querySelector("input").value);
    };
  } else if (t === "textarea") {
    const ta = el("textarea", { rows: 3, value: value ?? "" });
    control.append(ta);
    get = () => (ta.value === "" ? null : ta.value);
    set = (v) => (ta.value = v ?? "");
  } else if (NUMERIC.includes(t)) {
    const input = el("input", { type: "number", step: t === "int" ? "1" : "any", value: value ?? "" });
    control.append(input);
    get = () => (input.value === "" ? null : Number(input.value));
    set = (v) => (input.value = v ?? "");
  } else if (t === "date") {
    const input = el("input", { type: "date", value: value ?? "" });
    control.append(input);
    get = () => input.value || null;
    set = (v) => (input.value = v || "");
  } else if (t === "datetime") {
    const input = el("input", { type: "datetime-local", step: "1", value: value ? value.slice(0, 19) : "" });
    control.append(el("div", { style: "display:flex;gap:6px;align-items:center" }, input, el("span", { class: "faint" }, "UTC")));
    get = () => (input.value ? (input.value.length === 16 ? input.value + ":00" : input.value) + "Z" : null);
    set = (v) => (input.value = v ? String(v).slice(0, 19) : "");
  } else if (t === "reference") {
    const parent = rec && f.relationshipName && rec[f.relationshipName];
    const lookup = lookupControl(f, value, parent && f.referenceNameField ? parent[f.referenceNameField] : null);
    control.append(lookup.el);
    get = lookup.get;
    set = lookup.set;
  } else {
    const type = { email: "email", phone: "tel", url: "url" }[t] || "text";
    const input = el("input", { type, value: value ?? "", maxlength: f.length || null });
    control.append(input);
    get = () => (input.value === "" ? null : input.value);
    set = (v) => (input.value = v ?? "");
  }
  if (f.description) control.append(el("div", { class: "hint" }, f.description));
  controls[f.name] = { get, set, row };
  return row;
}

/** A lookup field: type to search the parent object by name, or paste an id. */
function lookupControl(f, value, name) {
  let current = value || null;
  const target = S.objects.find((o) => o.name === f.referenceTo);
  const nf = target && target.nameField;
  const input = el("input", { type: "text", placeholder: `Search ${target ? target.labelPlural : f.referenceTo}…`, value: name || value || "" });
  const menu = el("div", { class: "lookup-menu", hidden: true });
  const open = el("a", { href: current ? `#/data/${f.referenceTo}/${current}` : "#", class: "hint", hidden: !current }, "Open ›");
  const wrap = el("div", { class: "lookup" }, input, menu);
  let hl = -1;
  let items = [];
  const choose = (item) => {
    current = item ? item.Id : null;
    input.value = item ? item[nf] || item.Id : "";
    menu.hidden = true;
    open.hidden = !current;
    if (current) open.href = `#/data/${f.referenceTo}/${current}`;
    input.dispatchEvent(new Event("change", { bubbles: true }));
  };
  const search = debounce(async () => {
    const q = input.value.trim();
    if (!q) {
      choose(null);
      return;
    }
    const conds = [];
    if (/^[a-zA-Z0-9]{15}([a-zA-Z0-9]{3})?$/.test(q)) conds.push(`Id = ${soqlString(q)}`);
    if (nf) conds.push(`${nf} LIKE ${soqlString("%" + q.replace(/[%_]/g, "\\$&") + "%")}`);
    if (!conds.length) return;
    const soql = `SELECT Id${nf ? ", " + nf : ""} FROM ${f.referenceTo} WHERE ${conds.join(" OR ")}${nf ? " ORDER BY " + nf : ""} LIMIT 8`;
    const res = await rest("GET", "/query?q=" + encodeURIComponent(soql));
    items = res.status === 200 ? res.body.records : [];
    hl = items.length ? 0 : -1;
    fill(menu, 
      ...(items.length
        ? items.map((it, i) => el("div", { class: i === hl ? "hl" : "", onmousedown: (e) => { e.preventDefault(); choose(it); } }, el("span", {}, it[nf] || it.Id), el("span", { class: "mono" }, it.Id)))
        : [el("div", { class: "muted" }, "No matches")]),
    );
    menu.hidden = false;
  }, 180);
  input.addEventListener("input", (e) => {
    e.stopPropagation();
    search();
  });
  input.addEventListener("keydown", (e) => {
    if (menu.hidden || !items.length) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      hl = (hl + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length;
      [...menu.children].forEach((c, i) => c.classList.toggle("hl", i === hl));
    } else if (e.key === "Enter") {
      e.preventDefault();
      choose(items[hl]);
    } else if (e.key === "Escape") {
      menu.hidden = true;
    }
  });
  input.addEventListener("blur", () => setTimeout(() => (menu.hidden = true), 120));
  return {
    el: el("div", {}, wrap, open),
    get: () => current,
    set: (v) => {
      current = v || null;
      input.value = v || "";
      open.hidden = !current;
    },
  };
}

/* ----- fields tab ----- */

const FIELD_TYPES = [
  ["text", "Text"],
  ["textarea", "Text Area"],
  ["longtext", "Long Text Area"],
  ["email", "Email"],
  ["phone", "Phone"],
  ["url", "URL"],
  ["picklist", "Picklist"],
  ["multipicklist", "Picklist (Multi-Select)"],
  ["checkbox", "Checkbox"],
  ["number", "Number"],
  ["currency", "Currency"],
  ["percent", "Percent"],
  ["date", "Date"],
  ["datetime", "Date/Time"],
  ["lookup", "Lookup Relationship"],
  ["masterdetail", "Master-Detail Relationship"],
  ["autonumber", "Auto Number"],
  ["formula", "Formula"],
];

function uiTypeOf(f) {
  if (f.kind === "autonumber") return "autonumber";
  if (f.kind === "formula") return "formula";
  switch (f.describeType) {
    case "string": return "text";
    case "textarea": return (f.length || 0) > 255 ? "longtext" : "textarea";
    case "boolean": return "checkbox";
    case "int": case "double": return "number";
    case "reference": return f.masterDetail ? "masterdetail" : "lookup";
    default: return f.describeType;
  }
}

function typeLabel(f) {
  const base = (FIELD_TYPES.find(([k]) => k === uiTypeOf(f)) || [, f.describeType])[1];
  let detail = "";
  if (f.kind === "system") return el("span", { class: "field-type" }, base, el("span", { class: "detail" }, " · system"));
  if (f.kind === "compound") return el("span", { class: "field-type" }, base, el("span", { class: "detail" }, " · First + Last name"));
  if (f.describeType === "reference") detail = ` → ${f.referenceTo}`;
  else if (["string", "textarea", "email", "url", "phone"].includes(f.describeType) && f.length && f.kind !== "autonumber") detail = ` (${f.length})`;
  else if (f.describeType === "picklist" || f.describeType === "multipicklist") detail = ` · ${f.values.length} values${f.restricted ? ", restricted" : ""}`;
  else if (f.kind === "autonumber") detail = ` · ${f.autoNumberFormat}`;
  else if (f.kind === "formula") detail = ` · ${f.describeType}`;
  return el("span", { class: "field-type" }, base, el("span", { class: "detail" }, detail));
}

function renderFields(pane) {
  const s = S.schema;
  const toolbar = el(
    "div",
    { class: "toolbar" },
    el("span", { class: "muted" }, s.editable ? "This object was created here: add, edit and delete its fields." : `This object comes from ${s.sourceName}. Fields you add here are stored in the org.`),
    el("div", { class: "spacer" }),
    el("button", { class: "btn primary", onclick: () => fieldDialog(null) }, "+ New field"),
  );
  const flag = (on, text) => (on ? el("span", { class: "badge" }, text) : null);
  const table = el(
    "table",
    { class: "grid" },
    el("thead", {}, el("tr", {}, ["Label", "API name", "Type", "", "Defined in", ""].map((h) => el("th", {}, h)))),
    el(
      "tbody",
      {},
      s.fields.map((f) =>
        el(
          "tr",
          {},
          el("td", { title: f.description || "" }, f.label),
          el("td", { class: "mono" }, f.name),
          el("td", {}, typeLabel(f)),
          el("td", {}, flag(f.required && f.kind !== "system", "Required"), " ", flag(f.unique, "Unique"), " ", flag(f.externalId, "External ID"), " ", flag(f.nameField, "Name")),
          el("td", {}, sourceBadge(f.source)),
          el(
            "td",
            {},
            f.editable
              ? [el("button", { class: "btn small", onclick: () => fieldDialog(f) }, "Edit"), " ", el("button", { class: "btn small danger", onclick: () => deleteField(f) }, "Delete")]
              : null,
          ),
        ),
      ),
    ),
  );
  const children = s.childRelationships.length
    ? [
        el("div", { class: "section-title", style: "margin:20px 18px 6px" }, "Child relationships (for subqueries)"),
        el(
          "table",
          { class: "grid" },
          el("thead", {}, el("tr", {}, ["Relationship name", "Child object", "Field", "On delete"].map((h) => el("th", {}, h)))),
          el(
            "tbody",
            {},
            s.childRelationships.map((c) =>
              el("tr", {}, el("td", { class: "mono" }, c.name), el("td", {}, el("a", { href: `#/data/${c.childObject}/fields` }, c.childObject)), el("td", { class: "mono" }, c.field), el("td", {}, c.cascadeDelete ? "Cascade" : "Clear or restrict")),
            ),
          ),
        ),
      ]
    : null;
  pane.append(toolbar, el("div", { class: "pane-body" }, table, children));
}

async function deleteField(f) {
  if (!(await confirmDialog("Delete field", `Delete ${S.schema.name}.${f.name}? Its data is deleted too.`, "Delete field"))) return;
  try {
    await api(`/objects/${S.schema.name}/fields/${f.name}`, { method: "DELETE" });
  } catch (e) {
    return toast(e.message, true);
  }
  store("cols." + S.schema.name, null);
  toast("Field deleted");
  await afterChange();
  renderData();
}

function fieldDialog(existing) {
  const s = S.schema;
  const editing = !!existing;
  const v = existing || {};
  const inputs = {};
  const mk = (name, node) => (inputs[name] = node);
  const row = (label, node, hint, opts = {}) =>
    el("div", { class: "form-row", "data-for": opts.for || "" }, el("label", {}, label), el("div", { class: "control" }, node, hint ? el("div", { class: "hint" }, hint) : null));

  const type = mk("type", el("select", { disabled: editing }, FIELD_TYPES.map(([k, l]) => el("option", { value: k, selected: k === (editing ? uiTypeOf(v) : "text") }, l))));
  const label = mk("label", el("input", { type: "text", value: v.label || "" }));
  const name = mk("name", el("input", { type: "text", value: v.name || "", disabled: editing, class: "mono" }));
  let nameTouched = editing;
  label.addEventListener("input", () => {
    if (!nameTouched) name.value = label.value ? apiNameFromLabel(label.value) : "";
  });
  name.addEventListener("input", () => (nameTouched = true));
  const description = mk("description", el("textarea", { rows: 2, value: v.description || "" }));
  const length = mk("length", el("input", { type: "number", min: 1, value: v.length || "" }));
  const precision = mk("precision", el("input", { type: "number", min: 1, max: 18, value: v.precision || 18 }));
  const scale = mk("scale", el("input", { type: "number", min: 0, max: 17, value: v.scale ?? 2 }));
  const required = mk("required", el("input", { type: "checkbox", checked: v.required }));
  const unique = mk("unique", el("input", { type: "checkbox", checked: v.unique }));
  const externalId = mk("externalId", el("input", { type: "checkbox", checked: v.externalId }));
  const values = mk("values", el("textarea", { rows: 5, value: (v.values || []).join("\n"), placeholder: "One value per line" }));
  const restricted = mk("restricted", el("input", { type: "checkbox", checked: editing ? v.restricted : true }));
  const def = mk("default", el("input", { type: "text", value: v.default == null ? "" : String(v.default) }));
  const defBool = mk("defBool", el("input", { type: "checkbox", checked: v.default === true }));
  const defFormula = mk("defFormula", el("input", { type: "text", class: "mono", value: v.defaultFormula || "", placeholder: "e.g. TODAY() + 30" }));
  const refTo = mk(
    "referenceTo",
    el("select", { disabled: editing }, S.objects.map((o) => el("option", { value: o.name, selected: o.name === (v.referenceTo || "Account") }, `${o.label} (${o.name})`))),
  );
  const childRel = mk("childRel", el("input", { type: "text", class: "mono", value: v.childRelationshipName || "" }));
  const restrict = mk("restrict", el("input", { type: "checkbox", checked: v.restrictDelete }));
  const autoFormat = mk("autoFormat", el("input", { type: "text", class: "mono", value: v.autoNumberFormat || "A-{0000}" }));
  const formula = mk("formula", el("textarea", { rows: 3, class: "mono", value: v.formula || "" }));
  const formulaType = mk(
    "formulaType",
    el("select", { disabled: editing }, [["string", "Text"], ["double", "Number"], ["currency", "Currency"], ["percent", "Percent"], ["boolean", "Checkbox"], ["date", "Date"], ["datetime", "Date/Time"]].map(([k, l]) => el("option", { value: k, selected: k === v.type }, l))),
  );
  const defaultChildRel = () => {
    const plural = s.labelPlural.replace(/[^A-Za-z0-9]/g, "");
    return plural + "__r";
  };
  if (!editing) childRel.value = defaultChildRel();

  const err = el("div", {});
  const body = el(
    "div",
    {},
    err,
    row("Type", type),
    row("Label", label),
    row("API name", name, editing ? null : "Ends in __c. Can't be changed later."),
    row("Length", length, null, { for: "text longtext" }),
    row("Precision", precision, "Total digits", { for: "number currency percent" }),
    row("Decimal places", scale, null, { for: "number currency percent" }),
    row("Values", values, "One per line. The default is chosen below.", { for: "picklist multipicklist" }),
    row("Restricted", el("label", { class: "check" }, restricted, "Only these values are accepted"), null, { for: "picklist multipicklist" }),
    row("Related to", refTo, null, { for: "lookup masterdetail" }),
    row("Child relationship name", childRel, `For subqueries from the parent: (SELECT … FROM ${childRel.value || "Things__r"})`, { for: "lookup masterdetail" }),
    row("Delete protection", el("label", { class: "check" }, restrict, "Don't allow the parent to be deleted while this is set"), "Otherwise the field is cleared when the parent is deleted.", { for: "lookup" }),
    row("Display format", autoFormat, "e.g. INV-{0000} or {YYYY}{MM}-{000}", { for: "autonumber" }),
    row("Formula", formula, "Evaluated whenever the record is read, so it can be filtered and sorted on (not grouped). Salesforce's formula functions are supported: IF, CASE, ISBLANK, TEXT, LEFT, CONTAINS, REGEX, ROUND, TODAY, ADDMONTHS, VLOOKUP and the rest.", { for: "formula" }),
    row("Result type", formulaType, null, { for: "formula" }),
    row("Default value", def, "Text, number or picklist value", { for: "text email phone url number currency percent picklist" }),
    row("Default", el("label", { class: "check" }, defBool, "Checked"), null, { for: "checkbox" }),
    row("Default formula", defFormula, "Instead of a fixed default: a formula worked out when a record is created, such as TODAY() + 30 or $User.Email. It can't use the record's own fields.", { for: "text textarea longtext email phone url number currency percent date datetime" }),
    row("Options", el("div", { class: "multi" },
      el("label", { class: "check", "data-opt": "required" }, required, "Required"),
      el("label", { class: "check", "data-opt": "unique" }, unique, "Unique"),
      el("label", { class: "check", "data-opt": "external" }, externalId, "External ID"),
    )),
    row("Description", description),
  );
  const sync = () => {
    const t = type.value;
    for (const r of body.querySelectorAll(".form-row[data-for]")) {
      const f = r.dataset.for;
      if (f) r.hidden = !f.split(" ").includes(t);
    }
    const textual = ["text", "email", "phone", "url", "number"].includes(t);
    body.querySelector("[data-opt=unique]").hidden = !textual;
    body.querySelector("[data-opt=external]").hidden = !textual;
    body.querySelector("[data-opt=required]").hidden = ["checkbox", "masterdetail", "autonumber", "formula"].includes(t);
    if (!editing && !length.value) length.value = t === "longtext" ? 32768 : 255;
    if (!editing && t === "longtext" && length.value === "255") length.value = 32768;
    if (!editing && t === "text" && length.value === "32768") length.value = 255;
  };
  type.addEventListener("change", sync);
  sync();

  let close;
  const submit = el("button", { class: "btn primary", onclick: doSubmit }, editing ? "Save" : "Create field");
  close = modal(editing ? `Edit ${v.label}` : `New field on ${s.label}`, body, [el("button", { class: "btn", onclick: () => close() }, "Cancel"), submit]);

  async function doSubmit() {
    const t = type.value;
    const fieldType = {
      text: "string", textarea: "textarea", longtext: "textarea", email: "email", phone: "phone", url: "url",
      picklist: "picklist", multipicklist: "multipicklist", checkbox: "boolean", number: "double", currency: "currency",
      percent: "percent", date: "date", datetime: "datetime", lookup: "reference", masterdetail: "reference",
      autonumber: "string", formula: formulaType.value,
    }[t];
    const vals = values.value.split("\n").map((x) => x.trim()).filter(Boolean);
    const num = (x) => (x.value === "" ? null : Number(x.value));
    let dflt = null;
    // A default formula (when its row is shown) takes the place of a fixed default.
    const dfFormula = defFormula.closest(".form-row").hidden ? "" : defFormula.value.trim();
    if (t === "checkbox") dflt = defBool.checked;
    else if (dfFormula) dflt = null;
    else if (def.value !== "" && !["lookup", "masterdetail", "autonumber", "formula", "date", "datetime", "textarea", "longtext", "multipicklist"].includes(t)) {
      dflt = ["number", "currency", "percent"].includes(t) ? Number(def.value) : def.value;
    }
    const payload = {
      name: name.value.trim(),
      type: fieldType,
      label: label.value.trim() || null,
      description: description.value.trim() || null,
      length: ["text", "longtext"].includes(t) ? num(length) : t === "textarea" ? 255 : null,
      precision: ["number", "currency", "percent"].includes(t) ? num(precision) : null,
      scale: ["number", "currency", "percent"].includes(t) ? num(scale) : null,
      required: required.checked && !body.querySelector("[data-opt=required]").hidden,
      reference_to: ["lookup", "masterdetail"].includes(t) ? refTo.value : null,
      child_relationship_name: ["lookup", "masterdetail"].includes(t) ? childRel.value.trim() || null : null,
      master_detail: t === "masterdetail",
      restrict_delete: t === "lookup" && restrict.checked,
      values: ["picklist", "multipicklist"].includes(t) ? vals : [],
      restricted: ["picklist", "multipicklist"].includes(t) && restricted.checked,
      default: dflt,
      default_formula: dfFormula || null,
      external_id: externalId.checked && !body.querySelector("[data-opt=external]").hidden,
      unique: unique.checked && !body.querySelector("[data-opt=unique]").hidden,
      auto_number_format: t === "autonumber" ? autoFormat.value.trim() : null,
      formula: t === "formula" ? formula.value.trim() || "null" : null,
    };
    if (!payload.name) return showErr("An API name is required.");
    submit.disabled = true;
    try {
      if (editing) await api(`/objects/${s.name}/fields/${v.name}`, { method: "PUT", body: payload });
      else await api(`/objects/${s.name}/fields`, { method: "POST", body: payload });
    } catch (e) {
      submit.disabled = false;
      return showErr(e.message);
    }
    close();
    toast(editing ? "Field saved" : `Field ${payload.name} created`);
    if (!editing) {
      // Show the new field in the table straight away.
      const cols = store("cols." + s.name);
      if (cols) store("cols." + s.name, cols + "," + payload.name);
    }
    await afterChange();
    renderData();
  }
  function showErr(m) {
    fill(err, el("div", { class: "alert err" }, m));
  }
}

function newObjectDialog() {
  const label = el("input", { type: "text", placeholder: "Invoice" });
  const plural = el("input", { type: "text", placeholder: "Invoices" });
  const name = el("input", { type: "text", class: "mono", placeholder: "Invoice__c" });
  const nameLabel = el("input", { type: "text", placeholder: "Invoice Name" });
  const nameType = el("select", {}, el("option", { value: "text" }, "Text"), el("option", { value: "auto" }, "Auto Number"));
  const format = el("input", { type: "text", class: "mono", value: "INV-{0000}" });
  const description = el("textarea", { rows: 2 });
  let nameTouched = false;
  let pluralTouched = false;
  let nameLabelTouched = false;
  label.addEventListener("input", () => {
    const l = label.value.trim();
    if (!nameTouched) name.value = l ? apiNameFromLabel(l) : "";
    if (!pluralTouched) plural.value = l ? (l.endsWith("s") ? l + "es" : l.endsWith("y") && !/[aeiou]y$/i.test(l) ? l.slice(0, -1) + "ies" : l + "s") : "";
    if (!nameLabelTouched) nameLabel.value = l ? l + " Name" : "";
  });
  name.addEventListener("input", () => (nameTouched = true));
  plural.addEventListener("input", () => (pluralTouched = true));
  nameLabel.addEventListener("input", () => (nameLabelTouched = true));
  const formatRow = el("div", { class: "form-row", hidden: true }, el("label", {}, "Display format"), el("div", { class: "control" }, format, el("div", { class: "hint" }, "e.g. INV-{0000}")));
  nameType.addEventListener("change", () => (formatRow.hidden = nameType.value !== "auto"));
  const err = el("div", {});
  const r = (l, n, hint) => el("div", { class: "form-row" }, el("label", {}, l), el("div", { class: "control" }, n, hint ? el("div", { class: "hint" }, hint) : null));
  const body = el(
    "div",
    {},
    err,
    r("Label", label),
    r("Plural label", plural),
    r("API name", name, "Ends in __c. Record ids get a key prefix derived from it."),
    r("Record name label", nameLabel),
    r("Record name type", nameType),
    formatRow,
    r("Description", description),
    el("p", { class: "hint" }, "Like a real custom object, it gets Id, Name, OwnerId, the audit fields and a recycle bin. It's stored in this org's database; export it as SFDX metadata from the object's metadata tab."),
  );
  let close;
  const create = el(
    "button",
    {
      class: "btn primary",
      onclick: async () => {
        const payload = {
          name: name.value.trim(),
          label: label.value.trim() || null,
          label_plural: plural.value.trim() || null,
          name_field_label: nameLabel.value.trim() || null,
          name_field_format: nameType.value === "auto" ? format.value.trim() : null,
          description: description.value.trim() || null,
          fields: [],
        };
        create.disabled = true;
        try {
          await api("/objects", { method: "POST", body: payload });
        } catch (e) {
          create.disabled = false;
          return fill(err, el("div", { class: "alert err" }, e.message));
        }
        close();
        toast(`${payload.name} created`);
        await refreshObjects();
        go(`#/data/${payload.name}/fields`);
      },
    },
    "Create object",
  );
  close = modal("New custom object", body, [el("button", { class: "btn", onclick: () => close() }, "Cancel"), create]);
}

async function deleteObject() {
  const s = S.schema;
  if (!(await confirmDialog("Delete object", `Delete ${s.name} and all ${s.count + s.deletedCount} of its records? This can't be undone.`, "Delete object"))) return;
  try {
    await api(`/objects/${s.name}`, { method: "DELETE" });
  } catch (e) {
    return toast(e.message, true);
  }
  toast(`${s.name} deleted`);
  await refreshObjects();
  go("#/data/Account");
}

/* ----- metadata tab ----- */

async function renderMetadata(pane) {
  const body = el("div", { class: "pane-body", style: "padding: 14px 18px" }, el("div", { class: "muted" }, "Loading…"));
  pane.append(body);
  const m = await api(`/objects/${S.schema.name}/metadata`);
  if (!m.files.length) {
    fill(body, el("div", { class: "empty" }, `${S.schema.name} has no custom fields, so there's no metadata to export.`));
    return;
  }
  fill(body, 
    el(
      "div",
      { class: "alert info" },
      "SFDX source files for this object's custom definitions. Save them under ",
      el("code", {}, m.root + "/"),
      " in your Salesforce project to deploy them, or point localforce at the project with ",
      el("code", {}, "--metadata"),
      ". Definitions in metadata take precedence over ones created here.",
    ),
    m.files.map((f) =>
      el(
        "div",
        {},
        el("div", { style: "display:flex;align-items:center;gap:8px;margin-top:14px" }, el("strong", { class: "mono" }, f.path), el("div", { class: "spacer", style: "flex:1" }), el("button", { class: "btn small", onclick: () => download(f.path.split("/").pop(), f.content) }, "Download")),
        codeBlock(f.content),
      ),
    ),
  );
}

/* ---------- SOQL view ---------- */

function renderQuery() {
  const Q = S.query;
  const root = $("view-query");
  if (!Q.text) Q.text = store("soql") || "SELECT Id, Name, Industry, Owner.Name, (SELECT Name, Email FROM Contacts)\nFROM Account\nORDER BY Name\nLIMIT 100";
  const ta = el("textarea", { spellcheck: "false", value: Q.text, oninput: (e) => (Q.text = e.target.value) });
  ta.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      run();
    }
  });
  const history = JSON.parse(store("soql.history") || "[]");
  const results = el("div", { class: "results" });
  const meta = el("span", { class: "muted" });
  fill(root, 
    el(
      "div",
      { class: "query-wrap" },
      el(
        "div",
        { class: "editor" },
        ta,
        el(
          "div",
          { class: "row" },
          el("button", { class: "btn primary", onclick: run }, "Run ", el("span", { class: "faint", style: "color:inherit;opacity:.75" }, "⌘↵")),
          el("label", { class: "check" }, el("input", { type: "checkbox", checked: Q.all, onchange: (e) => (Q.all = e.target.checked) }), "Include deleted (queryAll)"),
          el("label", { class: "check" }, el("input", { type: "checkbox", checked: Q.mode === "json", onchange: (e) => { Q.mode = e.target.checked ? "json" : "table"; showResult(); } }), "Raw JSON"),
          history.length
            ? el("select", { onchange: (e) => { if (e.target.value) { ta.value = Q.text = e.target.value; e.target.value = ""; } } }, el("option", { value: "" }, "History…"), history.map((h) => el("option", { value: h }, h.replace(/\s+/g, " ").slice(0, 90))))
            : null,
          el("div", { class: "spacer" }),
          meta,
        ),
      ),
      results,
    ),
  );
  ta.focus();
  showResult();

  async function run() {
    const soql = ta.value.trim();
    if (!soql) return;
    store("soql", soql);
    const h = [soql, ...history.filter((x) => x !== soql)].slice(0, 25);
    store("soql.history", JSON.stringify(h));
    meta.textContent = "Running…";
    try {
      Q.result = await api("/query", { method: "POST", body: { soql, all: Q.all } });
      Q.error = null;
    } catch (e) {
      Q.result = null;
      Q.error = e.data;
    }
    showResult();
  }

  function showResult() {
    if (Q.error) {
      meta.textContent = "";
      const errs = Array.isArray(Q.error) ? Q.error : [{ message: String(Q.error && Q.error.error) }];
      fill(results, el("div", { style: "padding: 0 18px" }, errs.map((e) => el("div", { class: "alert err" }, el("strong", { class: "mono" }, e.errorCode || "ERROR"), el("pre", { class: "mono" }, e.message)))));
      return;
    }
    const r = Q.result;
    if (!r) {
      fill(results, el("div", { class: "empty" }, "Run a query to see results. Queries use the same engine as /services/data/v62.0/query."));
      return;
    }
    meta.textContent = `${r.totalSize.toLocaleString()} ${r.totalSize === 1 ? "row" : "rows"} · ${r.ms} ms${r.truncated ? " · showing the first 50,000" : ""}`;
    if (Q.mode === "json") {
      fill(results, el("pre", { class: "json mono" }, JSON.stringify({ totalSize: r.totalSize, done: r.done, records: r.records }, null, 2)));
      return;
    }
    if (!r.records.length) {
      fill(results, el("div", { class: "empty" }, r.totalSize ? `COUNT() = ${r.totalSize}` : "No rows."));
      return;
    }
    fill(results, resultTable(r.records));
  }
}

/** Flatten parent objects into dotted columns; child subqueries become expandable cells. */
function flatten(rec, prefix = "", out = {}) {
  for (const [k, v] of Object.entries(rec)) {
    if (k === "attributes") continue;
    const key = prefix + k;
    if (v && typeof v === "object" && !Array.isArray(v)) {
      if ("records" in v && "totalSize" in v) out[key] = { __children: v };
      else flatten(v, key + ".", out);
    } else {
      out[key] = v;
    }
  }
  return out;
}

function resultTable(records) {
  const rows = records.map((r) => flatten(r));
  const cols = [];
  for (const r of rows) for (const k of Object.keys(r)) if (!cols.includes(k)) cols.push(k);
  const tbody = el("tbody", {});
  for (const [i, r] of rows.entries()) {
    const tr = el("tr", {});
    for (const c of cols) {
      const v = r[c];
      let content;
      if (v && v.__children) {
        content = el("span", { class: "subrecords", onclick: () => toggleNested(tr, v.__children, c, cols.length) }, `${v.__children.totalSize} ${v.__children.totalSize === 1 ? "record" : "records"} ▸`);
      } else if (v == null) {
        content = el("span", { class: "null" }, records[i] && c in rows[i] ? "null" : "");
      } else if (typeof v === "string" && /^[a-zA-Z0-9]{18}$/.test(v) && S.byPrefix[v.slice(0, 3)]) {
        content = recordLink(v);
      } else {
        content = typeof v === "object" ? JSON.stringify(v) : String(v);
      }
      tr.append(el("td", { class: typeof v === "number" ? "num" : "", title: typeof v === "string" ? v : "" }, content));
    }
    tbody.append(tr);
  }
  return el("table", { class: "grid" }, el("thead", {}, el("tr", {}, cols.map((c) => el("th", {}, c)))), tbody);
}

function toggleNested(tr, child, name, span) {
  const next = tr.nextElementSibling;
  if (next && next.dataset.nested === name) {
    next.remove();
    return;
  }
  const row = el("tr", { "data-nested": name }, el("td", { colspan: span, class: "nested" }, el("div", { class: "muted", style: "margin-bottom:4px" }, name), resultTable(child.records)));
  tr.after(row);
}

/* ---------- API log ---------- */

async function pollLog() {
  try {
    const fresh = await api("/requests?after=" + S.log.lastId);
    if (fresh.length) {
      S.log.entries.push(...fresh);
      S.log.entries = S.log.entries.slice(-500);
      S.log.lastId = fresh[fresh.length - 1].id;
      if (S.view === "log") renderLogList();
    }
  } catch (_) {
    /* server restarting */
  }
}

function startLog() {
  if (!S.log.timer) S.log.timer = setInterval(pollLog, 1500);
}
function stopLog() {
  clearInterval(S.log.timer);
  S.log.timer = null;
}

function renderLog() {
  const root = $("view-log");
  fill(root, 
    el(
      "div",
      { class: "log-list" },
      el(
        "div",
        { class: "toolbar", style: "background:var(--panel);border-bottom:1px solid var(--border)" },
        el("strong", {}, "Requests to the Salesforce APIs"),
        el("span", { class: "muted" }, "newest first · last 500"),
        el("div", { class: "spacer", style: "flex:1" }),
        el("label", { class: "check" }, el("input", { type: "checkbox", checked: S.log.showUi, onchange: (e) => { S.log.showUi = e.target.checked; renderLogList(); } }), "Include this UI's requests"),
        el("button", { class: "btn", onclick: async () => { await api("/requests", { method: "DELETE" }); S.log.entries = []; S.log.selected = null; renderLog(); } }, "Clear"),
      ),
      el("div", { class: "pane-body", id: "log-body" }),
    ),
    S.log.selected ? el("aside", { class: "log-detail", id: "log-detail" }) : null,
  );
  renderLogList();
  if (S.log.selected) showLogEntry(S.log.selected);
  pollLog();
}

function renderLogList() {
  const body = $("log-body");
  if (!body) return;
  const list = S.log.entries.filter((e) => S.log.showUi || !e.ui).slice().reverse();
  if (!list.length) {
    fill(body, 
      el("div", { class: "empty" }, "No requests yet. Point an app at this server (see “Connect an app”) and its API calls appear here, with the exact reason anything failed."),
    );
    return;
  }
  fill(body, 
    el(
      "table",
      { class: "grid" },
      el("thead", {}, el("tr", {}, ["Time", "Method", "Path", "Status", "Error", "ms"].map((h) => el("th", {}, h)))),
      el(
        "tbody",
        {},
        list.map((e) =>
          el(
            "tr",
            { class: "clickable" + (S.log.selected === e.id ? " selected" : ""), onclick: () => { S.log.selected = e.id; renderLog(); } },
            el("td", { class: "mono" }, e.time.slice(11, 23)),
            el("td", { class: "method" }, e.method),
            el("td", { class: "mono", title: decodeURIComponent(e.path) }, decodePath(e.path)),
            el("td", { class: "status s" + String(e.status)[0] }, e.status),
            el("td", { class: "mono" }, e.errorCode || ""),
            el("td", { class: "num" }, e.durationMs),
          ),
        ),
      ),
    ),
  );
}

function decodePath(p) {
  try {
    return decodeURIComponent(p.replace(/\+/g, " "));
  } catch (_) {
    return p;
  }
}

async function showLogEntry(id) {
  const box = $("log-detail");
  if (!box) return;
  let e;
  try {
    e = await api("/requests/" + id);
  } catch (_) {
    fill(box, el("div", { class: "empty" }, "This request is no longer in the log."));
    return;
  }
  const curl = [`curl -X ${e.method} '${location.origin}${e.path}'`, ...e.request_headers.filter(([k]) => ["content-type", "sforce-query-options"].includes(k)).map(([k, v]) => `-H '${k}: ${v}'`), "-H 'authorization: Bearer $TOKEN'"];
  if (e.request_body) curl.push(`--data '${e.request_body.replace(/'/g, "'\\''")}'`);
  fill(box, 
    el("div", { style: "display:flex;align-items:center;gap:8px" }, el("span", { class: "method" }, e.method), el("span", { class: "status s" + String(e.status)[0] }, e.status), el("span", { class: "muted" }, `${e.duration_ms} ms`), el("div", { style: "flex:1" }), el("button", { class: "btn small", onclick: () => copy(curl.join(" \\\n  ")) }, "Copy as curl"), el("button", { class: "icon-btn", onclick: () => { S.log.selected = null; renderLog(); } }, "✕")),
    el("pre", { class: "mono", style: "margin-top:8px" }, decodePath(e.path)),
    el("div", { class: "section-title" }, "Request headers"),
    el("div", { class: "kv" }, e.request_headers.flatMap(([k, v]) => [el("span", {}, k), el("span", {}, v)])),
    e.request_body ? [el("div", { class: "section-title" }, "Request body"), el("pre", { class: "mono" }, pretty(e.request_body))] : null,
    el("div", { class: "section-title" }, "Response body"),
    e.response_body ? el("pre", { class: "mono" }, pretty(e.response_body)) : el("div", { class: "muted" }, "(empty)"),
  );
}

/* ---------- connect dialog ---------- */

function connectDialog() {
  const i = S.info;
  const user = i.users.find((u) => u.id === S.userId) || i.users[0];
  const body = el(
    "div",
    {},
    el("p", {}, "Point your app's Salesforce client at this server instead of ", el("code", {}, "login.salesforce.com"), ". Any client id, secret and password is accepted; usernames must belong to a user below."),
    el("div", { class: "kv", style: "margin-bottom:12px" },
      el("span", {}, "Instance URL"), el("span", {}, i.httpUrl),
      el("span", {}, "HTTPS"), el("span", {}, i.httpsUrl),
      el("span", {}, "Certificate"), el("span", {}, i.certificate),
      el("span", {}, "API version"), el("span", {}, i.apiVersion + " (any version works)"),
      el("span", {}, "Org id"), el("span", {}, i.orgId),
      el("span", {}, "Users"), el("span", {}, i.users.map((u) => u.username).join(", ")),
      el("span", {}, "Storage"), el("span", {}, i.persistent ? i.dataDir : "in memory (lost on restart; use --data-dir to keep it)"),
      i.metadata.length ? [el("span", {}, "Metadata"), el("span", {}, i.metadata.join(", "))] : null,
    ),
    el("div", { class: "section-title" }, "Get an access token"),
    codeBlock(`curl -s ${i.httpUrl}/services/oauth2/token \\\n  -d grant_type=password -d client_id=any -d client_secret=any \\\n  -d username=${user.username} -d password=any`),
    el("div", { class: "section-title" }, "jsforce (Node.js)"),
    codeBlock(`const conn = new jsforce.Connection({ loginUrl: "${i.httpUrl}", version: "${i.apiVersion}" });\nawait conn.login("${user.username}", "any password");`),
    el("div", { class: "section-title" }, "simple-salesforce (Python, needs HTTPS)"),
    codeBlock(`import requests\nfrom simple_salesforce import Salesforce\n\ns = requests.Session()\ns.verify = "${i.certificate}"\ntoken = s.post("${i.httpsUrl}/services/oauth2/token", data={"grant_type": "client_credentials"}).json()\nsf = Salesforce(instance_url=token["instance_url"], session_id=token["access_token"], session=s, version="${i.apiVersion}")`),
    el("div", { class: "section-title" }, "Environment variables (if your app reads them)"),
    codeBlock(`SALESFORCE_LOGIN_URL=${i.httpUrl}\nSALESFORCE_INSTANCE_URL=${i.httpUrl}\nSALESFORCE_USERNAME=${user.username}\nSALESFORCE_PASSWORD=any`),
    el("div", { class: "section-title" }, "Danger zone"),
    el("button", {
      class: "btn danger",
      onclick: async () => {
        if (!(await confirmDialog("Delete all records", "Delete every record in this org except users? The schema is kept.", "Delete all records"))) return;
        await api("/reset", { method: "POST" });
        toast("All records deleted");
        await refreshObjects();
        if (S.view === "data") route();
      },
    }, "Delete all records"),
  );
  modal("Connect an app", body, null, { wide: true });
}

/* ---------- misc ---------- */

function debounce(fn, ms) {
  let t;
  return (...args) => {
    clearTimeout(t);
    t = setTimeout(() => fn(...args), ms);
  };
}

function renderThemeButton() {
  const dark = document.documentElement.dataset.theme === "dark";
  $("theme-btn").textContent = dark ? "☀" : "☾";
}

async function init() {
  $("theme-btn").addEventListener("click", () => {
    const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    store("theme", next);
    renderThemeButton();
  });
  renderThemeButton();
  $("connect-btn").addEventListener("click", connectDialog);
  S.info = await api("/info");
  $("version").textContent = "v" + S.info.version;
  const savedUser = store("user");
  const userId = S.info.users.some((u) => u.id === savedUser) ? savedUser : S.info.defaultUser;
  fill($("user-select"), ...S.info.users.map((u) => el("option", { value: u.id, selected: u.id === userId }, u.name || u.username)));
  $("user-select").addEventListener("change", async (e) => {
    store("user", e.target.value);
    await newSession(e.target.value);
    toast("Now acting as " + e.target.selectedOptions[0].textContent);
  });
  await newSession(userId);
  await refreshObjects();
  window.addEventListener("hashchange", route);
  await route();
}

init().catch((e) => {
  fill(document.querySelector("main"), el("div", { class: "empty" }, el("div", { class: "alert err" }, "localforce couldn't start the UI: " + e.message)));
});
