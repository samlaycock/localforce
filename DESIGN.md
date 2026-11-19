# localforce: feasibility and design

**Verdict: feasible.** A spike in this repository already serves real Salesforce
client libraries. [jsforce](https://jsforce.github.io/) 3.13 (over HTTP) and
[simple-salesforce](https://github.com/simple-salesforce/simple-salesforce)
(over HTTPS) both run full create, query, upsert, delete, describe, collections
and composite flows against it unmodified. The spike is about 4,000 lines of Rust.

Salesforce is a different kind of hard from Snowflake. localflake's difficulty
is the depth of one language. localforce's difficulty is breadth: many APIs, a
metadata-driven data model, and platform automation that we can't run. SOQL
itself is the easy part. It is a fraction of Snowflake SQL, and it compiles
cleanly onto SQLite.

## What the spike proves

| Area | Status |
|---|---|
| **Auth** | OAuth `password`, `client_credentials` and `refresh_token` grants; SOAP `login`/`logout` (jsforce's default); `/userinfo`, `/id/{org}/{user}`, `/revoke`; `INVALID_SESSION_ID` on revoked tokens |
| **REST sObjects** | Create, retrieve (`?fields=`), update, delete, upsert / get / delete by external id (201 / 200 `created:false` / 300), basic info |
| **Describe** | Global describe and sObject describe, generated from the schema, including `childRelationships` |
| **SOQL** | Parent paths at any depth (`Account.Owner.Name`), child subqueries (`(SELECT … FROM Contacts)`, per-parent `LIMIT`), semi-joins, `LIKE`, `IN`, `INCLUDES`/`EXCLUDES`, `NOT`, aggregates with `GROUP BY` / `HAVING`, `exprN` keys and aliases, `COUNT()`, date functions, date literals (`TODAY`, `LAST_N_DAYS:n`, `THIS_QUARTER`, …), `FIELDS(ALL/STANDARD/CUSTOM)`, `ORDER BY … NULLS FIRST/LAST`, `LIMIT`/`OFFSET`, `queryAll` |
| **Paging** | 2,000-record batches, `Sforce-Query-Options: batchSize`, `nextRecordsUrl` locators |
| **DML rules** | Required fields, field-level create/update access, type coercion, string length, restricted picklists, email format, reference integrity, unique / external id fields, defaults, `Name` compound fields, auto-numbers, audit fields, recycle bin with cascading master-detail deletes and cleared lookups |
| **Composite** | `/composite` with `@{ref.id}` references and `allOrNone`; sObject Collections create / update / delete / retrieve with `allOrNone` rollback |
| **Errors** | Salesforce's error codes and messages, including SOQL's `ERROR at Row:1:Column:N` caret format |
| **Schema** | Six standard objects (User, Account, Contact, Lead, Opportunity, Case), plus custom objects and custom fields from `localforce.toml` |
| **Transport** | HTTP and HTTPS side by side, with a generated certificate |

Run it with `cargo run`, then `cargo test` for the Rust tests. The SDK suites
(jsforce, simple-salesforce and go-salesforce, sharing one conformance spec)
are in `tests/`; see `tests/README.md`.

### SOQL fidelity details the spike handles

These are the details an emulator gets wrong if it maps SOQL onto SQL naively,
and that tests written against it would then silently depend on:

- `=` on text is case-insensitive. Ids are case-sensitive at 15 characters and
  case-insensitive at 18, so both forms work everywhere.
- `Industry != 'Retail'` **matches records where `Industry` is null**. So does
  `NOT IN`, while `<` doesn't. Every comparison is compiled as
  `IFNULL(expr, 0|1)` so that `NOT` stays two-valued, as it is in SOQL.
- Nulls sort first in both `ASC` and `DESC` unless the query says otherwise.
- Date fields reject quoted strings ("must be of type date and should not be
  enclosed in quotes"), and text fields reject bare numbers.
- An empty child subquery is `null`, not an empty list. A null lookup is
  `"Parent": null`.
- DML results in collections use `statusCode`; REST errors use `errorCode`.

## Client compatibility findings

- **simple-salesforce hard-codes `https://`** for the instance URL, and
  `https://{domain}.salesforce.com` for its login helpers. localforce must
  therefore serve TLS. Python clients connect with `session_id` +
  `instance_url`, after getting a token from localforce's `/services/oauth2/token`.
  A trusted local CA (mkcert-style) is needed for a smooth experience; a plain
  self-signed certificate means passing `verify=` to the client.
- **jsforce** takes `loginUrl` and `instanceUrl`, so it works over plain HTTP.
- **jsforce warns that SOAP `login()` retires in Summer '27 (API 65.0).** Keep
  supporting it for older code, but lead with OAuth in the docs.
- **The `sf` CLI is not verified yet.** It is the biggest open compatibility
  question. `sf org login access-token --instance-url` plus `sf data` commands
  would be a headline feature. Check what it calls on login (identity,
  `Organization` queries, limits) before committing to it.

## Architecture

The architecture mirrors the sibling projects: a single binary on axum and
rusqlite, a TOML config, Docker, and changesets.

```
          HTTP :9600 / HTTPS :9643
                   │
  ┌────────────────┼────────────────────────────┐
  │ OAuth · SOAP login · REST · Bulk 1.0/2.0 ·  │ routes.rs, bulk.rs
  │ (Pub/Sub gRPC) · /dev UI + inspector        │
  └────────────────┬────────────────────────────┘
                   │ one dispatcher for top-level, composite and batch subrequests
             rest::dispatch                       rest.rs (+ sosl.rs, tooling.rs)
          ┌────────┴─────────┐
   soql::run          store (DML rules,          soql/, store.rs
   parse → compile     standard-object logic,
   to SQLite SQL       validation rules)
          │                 │
          │   formula ──────┤ formulas, roll-ups and rules
          │   compile       │ as SQL expressions              formula.rs
          └──────► SQLite, 1 table per sObject
                   │
               schema::Catalog  ◄── built-in standard objects
                                ◄── localforce.toml
                                ◄── SFDX source metadata
                                ◄── the web UI
```

Key decisions:

1. **SQLite, one table per sObject, typed columns.** Text columns use
   `COLLATE NOCASE` and id columns use binary collation. Datetimes are stored
   as fixed-width UTC strings, so they sort and compare correctly. Persistence
   is optional (`--data-dir`), like localoidc.
2. **SOQL is compiled to SQL rather than interpreted.** Parent relationships
   become `LEFT JOIN`s. Child subqueries run as one batched `IN (…)` query per
   relationship, so there are no N+1 queries. Semi-joins become SQL subqueries.
   SQLite does the heavy lifting, which matters once Bulk API loads 100k+ rows.
3. **A hand-written SOQL parser.** No crate parses SOQL, and owning the parser
   lets errors match Salesforce's row/column format exactly.
4. **Every REST request runs inside a SQLite savepoint.** Composite and
   collections use nested savepoints, which gives `allOrNone` true rollback
   semantics.
5. **One request dispatcher.** `/composite` and `/composite/batch` reuse it,
   and so will `/composite/graph` and `/composite/tree`.

## The hard parts, and how to approach them

### 1. Org metadata

A real org has hundreds of standard objects, many with dozens of fields, plus
the team's own customisations. Hand-writing all of that would never end. Instead:

- **Read SFDX source format.** Load `force-app/**/objects/*/*.object-meta.xml`
  and `fields/*.field-meta.xml` straight from the team's repository. Every modern
  Salesforce team has this, so localforce would emulate *their* org with no
  extra config. This is the feature that makes the emulator worth adopting.
- **Curate a built-in standard catalogue** of the roughly 30 objects apps
  actually use: Account, Contact, Lead, Opportunity, OpportunityLineItem,
  Product2, Pricebook2, PricebookEntry, Case, Task, Event, Campaign,
  CampaignMember, User, Group, RecordType, ContentVersion, ContentDocument,
  ContentDocumentLink, Attachment, Note, Order, Contract, Asset, Organization
  and so on. Include their real quirks: polymorphic `WhoId`/`WhatId`/`OwnerId`,
  `TYPEOF`, `IsClosed`/`IsWon` derived from stage, Lead conversion.

  The sales objects are done (`src/sales.rs`), with their documented rules:
  - **Opportunity products.** An entry must be active and in the opportunity's
    price book; the first product sets that price book. Give either
    `UnitPrice` or `TotalPrice`, not both. A new quantity on its own keeps the
    total, and a new discount keeps the unit price. The products' totals
    become the opportunity's `Amount`, which then ignores direct writes.
  - **The rest.** Contact roles have one primary per opportunity. Campaign
    member and opportunity counts are kept on the campaign. A contract's end
    date is computed from its term. An order needs a price book before
    products and at least one product to activate. Once it's activated,
    its products can be edited but not added or removed. Assets need an
    account, a contact, or both. Error codes and messages follow Salesforce's
    docs, Help articles and user reports where they give them.
  
  Parents' summary fields are stored and recalculated after each child save,
  delete or undelete, because, unlike roll-up summary fields, they can also be
  written directly when there are no children. Quotes, Group, RecordType,
  files and notes aren't built yet.
- **Optionally snapshot from a real org.** A one-off script would save describe
  output as JSON fixtures for teams that want exact parity.

### 2. Platform automation (Apex, flows, validation rules, formulas)

localforce can't run Apex. Apex is a full language with its own runtime and
governor limits, and that is out of scope. In order of value:

- **A formula engine (done)** for formula fields and validation rules
  (`src/formula.rs`). Formulas compile to SQLite expressions over the record's
  row, with parent fields as correlated subqueries, so a formula field is
  never stale and works in SOQL `WHERE` and `ORDER BY` like a column.
  Validation rules run inside the save's savepoint, after the write, and see
  the earlier values through `ISCHANGED` and `PRIORVALUE`. Default-value
  formulas are evaluated on create, with no record to read. The functions are
  Salesforce's, including `VLOOKUP`, `DISTANCE` and the time functions; the
  Visualforce-only functions and the org-setup globals (`$Setup`, `$Label`,
  `$Permission`, custom metadata) aren't. As in Salesforce, formula fields
  can't be grouped in SOQL, and long text areas can't be filtered or sorted.
- **Roll-up summary fields (done)**, compiled to subqueries over the details
  in the same way, so they're always current.
- **Trigger hooks.** On insert, update or delete of an object, localforce
  would call a local HTTP endpoint the team writes. That endpoint could modify
  the records or reject them with an error. It lets a team stub the side effects
  their tests depend on.
- **`/services/apexrest/*` mocks**: declared responses or a proxy, in the same
  spirit as localmail's provider emulation.

### 3. API breadth

These are the APIs to add, in priority order:

1. **Bulk API 2.0 and 1.0 (done).** Jobs run synchronously, through the same
   DML and SOQL code as the REST API (`src/bulk.rs`).
2. **`/composite/tree`, `/composite/batch`, `/composite/graph` (done).**
3. **Change Data Capture and Platform Events over the Pub/Sub API** (gRPC with
   Avro payloads, via tonic). Event-driven integrations are nearly impossible to
   test against real orgs, so this is a strong differentiator. Every DML
   operation would emit CDC events. Platform events are inserts on `__e`
   objects. The CometD Streaming API can be added for legacy clients.
4. **SOSL (done)**: `/search` and `/parameterizedSearch` (`src/sosl.rs`).
   Search terms match whole words, as Salesforce's index does: text is split
   into words at anything that isn't a letter or digit, `*` and `?` are
   wildcards within a word, and a term with punctuation (a phone number, an
   e-mail address) is a phrase of its words. The matching Ids go to a SOQL
   query built from the `RETURNING` clause, so it behaves exactly like SOQL.
   Without an `ORDER BY`, records are ranked by relevance: more matches first,
   name-field matches counting most, then the most recently modified.
   Salesforce's exact ranking, stemming and synonyms aren't reproduced.
5. **Tooling API (done, read-only)**: the schema as `CustomObject`,
   `CustomField`, `EntityDefinition`, `FieldDefinition` and `ValidationRule`
   (`src/tooling.rs`). The Metadata API: only the slices the `sf` CLI needs, if any.

### 4. Developer experience

The sibling projects established this pattern:

- A `/dev` web UI with an object browser, record editor, SOQL console and schema view.
- A **request inspector** that shows each call and exactly why it failed.
- **Fault injection**: `UNABLE_TO_LOCK_ROW`, `REQUEST_LIMIT_EXCEEDED`,
  `INVALID_SESSION_ID`, 503s and slow responses.
- **A shifted clock**, which makes date literals and token expiry testable.
- **Seed data and reset**: `POST /dev/api/reset`, plus loading `sf data tree`
  JSON files at startup.
- **Multiple orgs** on `<org>.localhost`.

## Suggested roadmap

| Phase | Scope | Rough size |
|---|---|---|
| **0 (done)** | The spike: auth, REST sObjects, SOQL, describe, composite and collections | — |
| **0.5 (done)** | SFDX metadata loading (source and mdapi formats, global value sets); custom objects and fields created in a web UI, stored in the org and exportable as SFDX metadata; the UI (records, editor, schema, SOQL, API log) | — |
| **0.75 (done)** | The gaps the SDK suites exposed: formula fields, roll-ups and validation rules; Opportunity, Case, Task, Event and price book logic; polymorphic lookups and `TYPEOF`; `ROLLUP` / `CUBE`; SOSL; `composite/tree`, `batch` and `graph`; Bulk API 2.0 and 1.0; replication, recent items and `If-Modified-Since`; a read-only Tooling API | — |
| **1 (done)** | Authorization-code flow (PKCE, user picker), the user-agent flow and the JWT bearer flow; OpportunityLineItem, OpportunityContactRole, Campaign, CampaignMember, Contract, Order, OrderItem and Asset; a Docker image | — |
| **1.5 (done)** | Checked against jsforce's own integration suite (`tests/upstream`) and Salesforce's docs: standard fields from real orgs' describes, compound addresses, page and compact layouts, list views, query plans, tabs and theme, every `/limits` entry, the SOAP calls REST lacks (`convertLead`, `merge`, `undelete`, `describeTabs`), `readMetadata` for fields | — |
| **2: data pipelines** | Record types, picklist dependencies, field history, `sf` CLI compatibility | 3–4 weeks |
| **3: behaviour** | Trigger hooks, Apex REST mocks, fault injection, shifted clock | 3–5 weeks |
| **4: events** | Pub/Sub API with CDC and platform events, CometD | 3–4 weeks |

## Risks

- **The fidelity long tail.** Like localflake, an emulator is only as good as
  its known differences. Mitigation: a shared conformance spec run by each
  client library in CI, and recording request/response pairs from a real
  developer org to diff against.
- **Metadata parsing scope.** SFDX metadata has many field types and options;
  support the common ones and report unsupported metadata loudly rather than
  ignoring it.
- **Trademark.** The name says what it does, but avoid Salesforce branding,
  logos and look-alike UI in the `/dev` pages.
