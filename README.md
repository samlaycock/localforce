# localforce

[![CI](https://github.com/samlaycock/localforce/actions/workflows/ci.yml/badge.svg)](https://github.com/samlaycock/localforce/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/samlaycock/localforce)](https://github.com/samlaycock/localforce/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

A local Salesforce emulator for offline development and testing. It's a single
Rust binary (or Docker image) that speaks Salesforce's REST API and OAuth
endpoints, so your app's Salesforce client connects to it instead of
`login.salesforce.com`. No org, network access or code changes are needed. It's
a sibling of [localoidc](https://github.com/samlaycock/localoidc),
[localsaml](https://github.com/samlaycock/localsaml),
[localmail](https://github.com/samlaycock/localmail) and
[localflake](https://github.com/samlaycock/localflake).

- **The real API, with real behaviour.** sObject CRUD, upsert by external id,
  describe, SOQL (relationship paths, subqueries, aggregates, date literals),
  query paging, sObject Collections and `/composite`. Errors use Salesforce's
  codes and messages: `REQUIRED_FIELD_MISSING`, `INVALID_FIELD`,
  `MALFORMED_QUERY` with its row and column, and so on.
- **Your org's schema.** The common sales and service objects are built in,
  with their logic: opportunity products update the opportunity's amount,
  closing a case sets its closed date, and so on. Custom objects and fields
  come from your SFDX project (`--metadata ./force-app`), from
  `localforce.toml`, or from the web UI.
- **A web UI for your data.** Browse and edit records, define objects and
  fields, run SOQL, and see every API call your app makes, with the reason
  anything failed.
- **Tested with real clients.** jsforce (Node.js), simple-salesforce (Python)
  and go-salesforce (Go) run unmodified against it, through a shared
  conformance spec that also runs against a real org.

> [!WARNING]
> localforce is for development and testing only. It accepts any password and
> client secret, has no sharing model or field-level security, and doesn't run
> Apex, flows or triggers. Formulas support Salesforce's functions except the
> Visualforce ones and the org-setup globals (`$Setup`, `$Label`,
> `$Permission`, custom metadata). Test against a real org before relying on
> behaviour in production.

## Contents

- [Installation](#installation)
- [Quick start](#quick-start)
- [Test fixtures, backup and restore](#test-fixtures-backup-and-restore)
- [Connecting an app](#connecting-an-app)
- [The web UI](#the-web-ui)
- [Defining custom objects and fields](#defining-custom-objects-and-fields)
- [What's emulated](#whats-emulated)
- [Configuration](#configuration)
- [Contributing](#contributing)
- [License](#license)

## Installation

**Docker** (linux/amd64 and linux/arm64):

```sh
docker run --rm -p 9600:9600 -p 9643:9643 -v localforce:/data ghcr.io/samlaycock/localforce
```

The image is tagged `:<version>`, `:<major>.<minor>` (and `:<major>` from 1.0)
and `:latest`. It runs as a non-root user and keeps the org in the `/data`
volume. Pass flags after the image name, e.g. `docker run …
ghcr.io/samlaycock/localforce --strict-sessions`.

**From source** (needs a recent stable Rust toolchain):

```sh
cargo install --git https://github.com/samlaycock/localforce --locked
localforce
```

Or clone the repository and run `cargo run --release`.

localforce serves HTTP on port 9600 and HTTPS on port 9643, so it can run next
to localoidc (9400), localsaml (9401) and localmail (9402).

## Quick start

Start localforce with either command above. It prints its URLs on startup:

| What | URL |
|---|---|
| Web UI | http://localhost:9600/ |
| Instance / login URL | `http://localhost:9600` |
| HTTPS (for clients that require it) | `https://localhost:9643`, with a generated certificate |

To load your project's custom objects, pass its SFDX folder:

```sh
localforce --metadata ../my-app/force-app --data-dir .localforce
```

`--data-dir` keeps the org (records, and objects made in the UI) between
restarts. Without it, everything is in memory.

With Docker, mount your project and point localforce at its SFDX folder and
config file:

```sh
docker run --rm -p 9600:9600 -p 9643:9643 -v localforce:/data \
  -v "$PWD:/project:ro" \
  -e LOCALFORCE_METADATA=/project/force-app \
  -e LOCALFORCE_CONFIG=/project/localforce.toml \
  ghcr.io/samlaycock/localforce
```

## Test fixtures, backup and restore

Start an in-memory org with an account and its related contact using the
[included record tree](tests/fixtures/accounts.json).

```sh
localforce --seed tests/fixtures/accounts.json
```

Pass Salesforce record tree JSON files in dependency order. Each file has a
top-level `records` array. Give every record an `attributes.type` and a unique
`attributes.referenceId`. Nest children under their relationship name, or use
`@referenceId` in a lookup field to refer to a record loaded earlier. For
example, a second file can use `"AccountId": "@Acme"` to add another contact.

```sh
localforce --metadata ./force-app --seed fixtures/accounts.json --seed fixtures/contacts.json
```

The org is ready to serve requests only after every file passes the usual
record validation. A failure exits with an error and rolls back the whole
import. With `--data-dir`, the first successful import marks the org as
seeded. Later starts skip the seed files. To test a changed fixture, start an
in-memory org or use a fresh data directory. For CSV ingestion into a running
org, use the Bulk API.

To capture a baseline after setup through any client or the UI, download the
running org's database from a second shell.

```sh
curl --fail --silent --show-error http://localhost:9600/__localforce/api/backup -o baseline.db
```

Then start each CI job from an independent copy.

```sh
localforce --restore baseline.db
```

The snapshot includes record IDs, lookup relationships, audit fields, recycle
bin contents, ID and auto-number counters, and schema created in the UI. Supply
the same `--config` and `--metadata` inputs to restore file-defined schema and
login settings. Each process starts with fresh sessions, query cursors, request
logs and Bulk API jobs.

To keep the restored org between restarts, add `--data-dir ./test-org` on the
first start. Restore requires a directory with no existing `org.db`. On
subsequent starts use `localforce --data-dir ./test-org`. In Docker, mount
the backup read-only, set `LOCALFORCE_RESTORE` to its container path, and use
a fresh `/data` volume for each independent run.

## Connecting an app

Any client id, client secret and password are accepted. Usernames must belong
to a configured user; the default user is `admin@localforce.dev`. The UI's
**Connect an app** dialog shows these snippets filled in for your server.

```sh
# A token (the password, client_credentials, refresh_token, authorization_code
# and JWT bearer grants all work)
curl -s http://localhost:9600/services/oauth2/token \
  -d grant_type=password -d client_id=any -d client_secret=any \
  -d username=admin@localforce.dev -d password=any
```

```js
// jsforce
const conn = new jsforce.Connection({ loginUrl: "http://localhost:9600", version: "62.0" });
await conn.login("admin@localforce.dev", "any password");
```

```python
# simple-salesforce always uses https://, so use the HTTPS port and trust the certificate
s = requests.Session(); s.verify = "/path/to/localforce-cert.pem"   # printed on startup
token = s.post("https://localhost:9643/services/oauth2/token", data={"grant_type": "client_credentials"}).json()
sf = Salesforce(instance_url=token["instance_url"], session_id=token["access_token"], session=s, version="62.0")
```

Access tokens localforce didn't issue are accepted as the default user unless
you pass `--strict-sessions`. Revoked tokens always get `INVALID_SESSION_ID`.

### Browser and server-to-server logins

- **Web server flow** (authorization code, with or without PKCE) and the
  **user-agent flow** (`response_type=token`). Send the browser to
  `http://localhost:9600/services/oauth2/authorize?response_type=code&client_id=…&redirect_uri=…`.
  Instead of a login form, localforce shows a page listing the org's users;
  pick one and it redirects back with a code, or with a token in the URL
  fragment. Add `login_hint=<username>` to skip the page, which is handy in
  automated tests. Codes last 15 minutes and work once. The code exchange
  checks the `redirect_uri` and the PKCE `code_verifier`, and returns a
  refresh token.
- **JWT bearer flow.** The assertion's claims are checked:
  - `iss` must be present.
  - `sub` must be a configured user.
  - `aud` must be `https://login.salesforce.com`, `https://test.salesforce.com`,
    another `*.salesforce.com` host, or localforce's own URL.
  - `exp` must not have passed, allowing three minutes for clock skew.

  The signature isn't checked, because there's no connected app certificate
  to check it against. As with Salesforce, no refresh token is returned.

## The web UI

- **Data**: every object, with record counts. For each object:
  - **Records**: a sortable, searchable table of any size, with chosen
    columns, parent names on lookups and a recycle bin with restore. Click a
    record to edit it. Inputs match each field's type, and lookups have a
    search picker. Edits go through the REST API, so you see the same
    validation errors your app would. Related lists link to child records.
  - **Fields & relationships**: every field with its type, flags and where
    it's defined. Add fields here: text, number, currency, picklists,
    checkboxes, dates, lookups, master-detail, auto-numbers and so on.
  - **SFDX metadata**: the object's custom definitions as
    `.object-meta.xml` / `.field-meta.xml` files, ready to commit to your
    project.
- **New**: create a custom object.
- **SOQL**: run queries, with subquery results you can expand, a raw-JSON view
  and history.
- **API log**: the last 500 API calls with headers and bodies, error codes,
  timings and "copy as curl".
- **Acting as**: the user records are created and edited as.

## Defining custom objects and fields

There are three ways, and they can be combined:

1. **SFDX metadata** (recommended). Point `--metadata` (or `metadata = [...]` in
   the config) at a project or `force-app` folder. localforce reads
   `objects/<Object>/<Object>.object-meta.xml`, `fields/*.field-meta.xml`,
   the older `objects/*.object` format and `globalValueSets`. Supported field
   types are Text, Text Area (long and rich), Email, Phone, URL, Picklist
   (including global value sets), Multi-select Picklist, Checkbox, Number,
   Currency, Percent, Date, Date/Time, Lookup (including `deleteConstraint`),
   Master-Detail, Hierarchy and Auto Number. Formula fields are evaluated when
   records are read, so they can be filtered and sorted on in SOQL (not
   grouped, as in Salesforce); default values can be formulas too; roll-up
   summary fields (`COUNT`, `SUM`, `MIN`, `MAX`, with filters) stay current as
   detail records change. Validation rules (`validationRules/*.validationRule-meta.xml`)
   run on every save. A formula or rule that uses a function localforce doesn't
   have is reported at startup (the field reads as null; the rule is ignored).
   Platform events and custom metadata types are skipped, with a warning at startup.
2. **`localforce.toml`**. See [`localforce.example.toml`](localforce.example.toml).
3. **The web UI**. Objects and fields made here are stored in the org's
   database. Export them from the **SFDX metadata** tab to move them into your
   project. Once they're in metadata, the file's definition takes precedence
   and the data is kept.

Custom object record ids get a key prefix derived from the object's name, so
ids stay the same across restarts and machines.

## What's emulated

| Area | Supported |
|---|---|
| Auth | OAuth token (`password`, `client_credentials`, `refresh_token`, `authorization_code` with PKCE, JWT bearer), `/authorize` with a user picker (code and token responses), `/userinfo`, `/id`, `/revoke`; SOAP `login`/`logout` |
| SOAP | The Partner API calls REST has no equivalent for: `convertLead` (account, contact and opportunity, and the lead locked), `merge` (related records move to the master), `undelete`, `describeTabs`, plus `getUserInfo` and `getServerTimestamp`. The Metadata API's `readMetadata`, for `CustomField` only |
| sObjects | Create, retrieve (`?fields=`), update, delete, upsert / get / delete by external id, basic info with recent items, describe, global describe, `If-Modified-Since`, `updated` / `deleted` for replication, `/recent` |
| Standard objects | The standard fields a new org has (taken from real orgs' describes), with compound addresses (`BillingAddress`, `MailingAddress`, …), an opportunity's forecast category and fiscal period, and a case's contact details. User, Account, Contact, Lead, Opportunity (stages set `IsClosed`, `IsWon`, `Probability`; products set `Amount`), Case, Task and Event (polymorphic `WhatId` / `WhoId`), Product2, Pricebook2 (with the standard price book), PricebookEntry, OpportunityLineItem (price book and price rules), OpportunityContactRole (one primary), Campaign and CampaignMember (member and opportunity counts), Contract (end date from the term), Order and OrderItem (totals, activation stops products being added or removed) and Asset |
| SOQL | Parent paths, polymorphic lookups and `TYPEOF`, child subqueries, semi-joins, `LIKE`, `IN`, `INCLUDES`/`EXCLUDES`, aggregates with `GROUP BY` (`ROLLUP`, `CUBE`, `GROUPING`) / `HAVING`, date functions and literals, `FIELDS()`, `ORDER BY … NULLS`, `LIMIT`/`OFFSET`, `FOR VIEW`, `queryAll`, `nextRecordsUrl` paging and `Sforce-Query-Options` |
| SOSL | `/search` (`FIND {…} IN … FIELDS RETURNING …` with fields, `WHERE`, `ORDER BY`, `LIMIT`) and `/parameterizedSearch`. Terms match whole words, with `*` and `?` wildcards; results are ordered by relevance |
| Composite | `/composite` with `@{ref}` references and `allOrNone`; `/composite/tree`, `/composite/batch`, `/composite/graph`; sObject Collections create / update / delete / retrieve |
| Bulk | Bulk API 2.0 ingest and query jobs (CSV); Bulk API 1.0 jobs and batches (CSV, JSON, XML) |
| Tooling | `/tooling/query` and describes for `ApexClass` (always empty), `CustomObject`, `CustomField`, `EntityDefinition`, `FieldDefinition` and `ValidationRule`; `EntityDefinition` and `FieldDefinition` through `/query` too |
| Data rules | Required fields, field access, types, lengths, restricted picklists, emails, reference integrity, unique and external ids, defaults, auto-numbers, compound names, formula and roll-up summary fields, validation rules, audit fields, recycle bin, cascading and restricted deletes |
| UI metadata | Page layouts generated from each object's fields (`describe/layouts`, `namedLayouts`, `compactLayouts`, `approvalLayouts`); list views (each object's standard views, their describe and results); query plans (`?explain=` for a query or list view); `/tabs` and `/theme` |
| Limits | `/limits` (every limit a Developer Edition org reports) and the `Sforce-Limit-Info` header |

Not emulated yet: streaming and events, the Metadata API (beyond reading fields), approval processes, Apex (including Apex
REST), triggers and flows. See [DESIGN.md](DESIGN.md) for the roadmap.

## Configuration

Settings come from flags or `LOCALFORCE_*` environment variables. Users, custom
objects and custom fields come from `localforce.toml` (see
[`localforce.example.toml`](localforce.example.toml)).

| Flag | Environment variable | Default | |
|---|---|---|---|
| `--host` | `LOCALFORCE_HOST` | `127.0.0.1` | Interface to listen on |
| `--port` | `LOCALFORCE_PORT` | `9600` | HTTP: the APIs, OAuth and the web UI |
| `--tls-port` | `LOCALFORCE_TLS_PORT` | `9643` | HTTPS, with a generated certificate |
| `--data-dir` | `LOCALFORCE_DATA_DIR` | none (in memory) | Where the org is kept between restarts |
| `--seed` | `LOCALFORCE_SEED` | none | Record tree JSON files, comma-separated or repeated, loaded once per org |
| `--restore` | `LOCALFORCE_RESTORE` | none | SQLite backup to use as the initial org |
| `--config` | `LOCALFORCE_CONFIG` | `localforce.toml` | Users, custom objects and custom fields |
| `--metadata` | `LOCALFORCE_METADATA` | none | SFDX project or metadata folders, comma-separated or repeated |
| `--strict-sessions` | `LOCALFORCE_STRICT_SESSIONS` | off | Reject access tokens localforce didn't issue |

### State

- Without `--data-dir`, the org is in memory and every start is a clean org.
- With it, records and objects made in the UI are kept in the SQLite database
  `org.db` there. Delete the file to start fresh.
- The HTTPS certificate is generated on the first start and kept in
  `--data-dir` (or in `localforce` under the system temp directory), so it only
  needs trusting once. Its path is printed on startup.

### Docker

- In the image, the org and the HTTPS certificate live in `/data`
  (`LOCALFORCE_DATA_DIR`), and localforce binds `0.0.0.0` (`LOCALFORCE_HOST`).
- From other containers, use the localforce container's name as the host, e.g.
  `http://localforce:9600`. The `instance_url` it returns follows the host you
  used. The certificate only names `localhost` and `127.0.0.1`, so use HTTP
  between containers.

## Contributing

Issues and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md)
for how to build, test and release, and how the SDK conformance spec works.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

## Not affiliated with Salesforce

"Salesforce" is a trademark of Salesforce, Inc. This project is independent and
interoperates with Salesforce's public APIs and the open-source clients that use
them.
