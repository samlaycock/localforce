# localforce

A local Salesforce emulator for offline development and testing. It's a single
Rust binary that speaks Salesforce's REST API and OAuth endpoints, so your app's
Salesforce client connects to it instead of `login.salesforce.com`. No org,
network access or code changes are needed.

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

## Quick start

```sh
cargo run --release
```

The server prints its URLs on startup:

| What | URL |
|---|---|
| Web UI | http://localhost:9600/ |
| Instance / login URL | `http://localhost:9600` |
| HTTPS (for clients that require it) | `https://localhost:9643`, with a generated certificate |

To load your project's custom objects, pass its SFDX folder:

```sh
cargo run --release -- --metadata ../my-app/force-app --data-dir .localforce
```

`--data-dir` keeps the org (records, and objects made in the UI) between
restarts. Without it, everything is in memory.

### With Docker

```sh
docker build -t localforce .
docker run -p 9600:9600 -p 9643:9643 localforce
```

The image keeps the org in `/data` (a volume). To load an SFDX project and a
config file, mount them and point localforce at them:

```sh
docker run -p 9600:9600 -p 9643:9643 \
  -v "$PWD:/project:ro" \
  -e LOCALFORCE_METADATA=/project/force-app \
  -e LOCALFORCE_CONFIG=/project/localforce.toml \
  localforce
```

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

## Development

```sh
cargo test                                   # Rust tests
cargo clippy --all-targets -- -D warnings
LOCALFORCE_UI_DIR=ui cargo run               # serve ui/ from disk: edit and reload
```

localforce is tested through the most-used Salesforce client in each of
three languages: jsforce (Node.js), simple-salesforce (Python) and
go-salesforce (Go). Their suites share a conformance spec of over 250 cases
written against Salesforce's behaviour, so the same cases also run against a
real org, plus `todo` cases for what isn't emulated yet. See
[tests/README.md](tests/README.md):

```sh
cargo build
(cd tests/node && npm ci && npm test)
(cd tests/python && uv run pytest)
(cd tests/go && go test -count=1 ./...)
```
