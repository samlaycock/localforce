# SDK integration tests

localforce is tested through the Salesforce client most used in each of three
languages. Each has an integration suite that runs against a real localforce
server:

| Language | SDK | Suite | Version |
|---|---|---|---|
| Node.js | [jsforce](https://jsforce.github.io/) | `tests/node` (`node --test`) | 3.x (pinned in `package-lock.json`) |
| Python | [simple-salesforce](https://github.com/simple-salesforce/simple-salesforce) | `tests/python` (pytest, uv) | 1.12.x (pinned in `uv.lock`) |
| Go | [go-salesforce](https://github.com/k-capehart/go-salesforce) | `tests/go` (`go test`) | v3 (pinned in `go.mod`) |

Salesforce publishes few official SDKs for its REST API, so these are the
community clients that apps actually use. Salesforce's own CLI is built on
jsforce.

```sh
cargo build                                  # the suites start target/debug/localforce
(cd tests/node && npm ci && npm test)
(cd tests/python && uv run pytest)
(cd tests/go && go test -count=1 ./...)      # -count=1: results depend on the server binary
```

Set `LOCALFORCE_BIN` to test another binary. Each suite starts its own server
on free ports with the fixture schema and users, a temporary data directory
and `--strict-sessions` (so, like Salesforce, tokens it didn't issue are
refused), and stops it afterwards.

`tests/api.rs` and `tests/schema.rs` are the Rust tests (`cargo test`).

## Shared conformance spec

Most coverage lives in `tests/conformance/cases/*.json`, which every suite runs
through its SDK (`conformance.test.mjs`, `test_conformance.py`,
`conformance_test.go`). A case covers every SDK at once, so the suites can't
drift apart. The cases describe Salesforce's behaviour, so they also run
against a real org. See `tests/conformance/README.md` for the format.

| File | Covers |
|---|---|
| `01-api` | Version list and resources, unsupported versions, older versions' URLs, `/limits`, `Sforce-Limit-Info`, `INVALID_SESSION_ID`, userinfo, OAuth errors, malformed JSON, `METHOD_NOT_ALLOWED`, the shape of errors |
| `02-describe` | Global describe and key prefixes; standard and custom fields (types, lengths, precision, picklists, defaults, external ids, nillable / createable / updateable); lookups, master-detail, formulas, auto-numbers, child relationships, restricted deletes |
| `03-crud` | Create / retrieve / update / delete; defaults, owner and audit fields; typed round-trips; UTC datetimes; compound names; auto-numbers; 15- and 18-character ids; `?fields=`; the recycle bin; status codes |
| `04-upsert` | Upsert by external id (create, update, ignoring case, 201 then 200), by Id, in batches, 300 Multiple Choices; GET and DELETE by external id |
| `05-validation` | Required fields (every one named), unknown and read-only fields, non-reparentable master-detail, string lengths, restricted picklists, emails, number / date / boolean types, `MALFORMED_ID`, wrong-object and deleted references, uniqueness, atomic failures |
| `06-soql-filters` | Field selection, SOQL's null and case rules, comparisons, `IN`, `LIKE` with escapes, escaped strings, `INCLUDES` / `EXCLUDES`, boolean logic, ordering with `NULLS FIRST / LAST`, `LIMIT` / `OFFSET`, `FIELDS()`, `COUNT()`, long text areas (not filterable or sortable), query errors with row and column |
| `07-soql-relationships` | Parent paths up to three levels, null parents, filtering and ordering by parent fields, child subqueries (per-parent `WHERE` / `ORDER BY` / `LIMIT`, null when empty, several at once), semi-joins and anti-joins |
| `08-soql-aggregates` | `COUNT` / `COUNT_DISTINCT` / `SUM` / `AVG` / `MIN` / `MAX`, `exprN` and aliases, `GROUP BY` (several fields, parent fields, `ROLLUP`, `CUBE`), `GROUPING()`, `HAVING`, ordering by aggregates, date functions, fields that can't be grouped (formulas, roll-ups, fractional numbers, datetimes) |
| `09-soql-dates` | Date and datetime values in any offset, date literals (`TODAY`, `LAST_N_DAYS:n`, `N_DAYS_AGO:n`, calendar periods), literals against datetime fields, type errors |
| `10-query-paging` | 2,000-record pages, `nextRecordsUrl`, `Sforce-Query-Options: batchSize`, the SDK following every page, reading a locator twice, invalid locators, `queryAll` |
| `11-delete-behaviour` | Master-detail cascades, cleared lookups, restricting lookups (`DELETE_FAILED`), Account → Contact / Opportunity cascades, partial and all-or-none multi-record deletes |
| `12-collections` | sObject Collections: create / update / delete / retrieve several records, `allOrNone`, mixed object types, the 200-record limit, nulls for missing ids |
| `13-composite` | `@{ref}` references (ids and query results), CRUD, describe and upsert subrequests, `allOrNone` with `PROCESSING_HALTED`, partial success, the 25-subrequest limit, duplicate reference ids; `/composite/tree` (nested children, all-or-none, type checks), `/composite/batch` (`haltOnError`), `/composite/graph` |
| `14-standard-objects` | User, Lead, Opportunity (stages set `IsClosed`, `IsWon`, `Probability`, forecast category), Case (`CaseNumber`, `ClosedDate`), Contact, and price books (the standard price book, standard prices first, one price per book) |
| `15-formulas` | Formula fields (number, checkbox, text, date and parent-field formulas; blank-as-zero and blank-as-blank; division by zero) in records and SOQL `WHERE` / `ORDER BY`; roll-up summaries (`SUM`, filtered `COUNT`) as details change; default-value formulas; describe |
| `16-validation-rules` | Validation rules on create, update and upsert, `REGEX`, `ISNEW` / `ISCHANGED` / `PRIORVALUE`, inactive rules, in collections (with and without `allOrNone`) and composite |
| `17-activities` | Task and Event defaults and derived fields, polymorphic `What` / `Who` (`What.Name`, `What.Type`, filters), `TYPEOF`, `Tasks` / `Events` child relationships, which objects each lookup accepts |
| `18-search` | SOSL through `/search` (terms, `AND` / `OR`, wildcards, `IN … FIELDS`, `RETURNING` with fields, `WHERE`, `ORDER BY`, `LIMIT`), whole-word matching, phone numbers and e-mail addresses as phrases, `/parameterizedSearch` (GET and POST), search errors |
| `19-replication-and-recent` | `sobjects/{type}/updated` and `/deleted`, `/recent` and `FOR VIEW`, `If-Modified-Since` on describes and records |
| `20-tooling` | Tooling API queries (`ApexClass`, `ValidationRule`, `CustomObject`, `CustomField`), tooling describes, `EntityDefinition` and `FieldDefinition` through the data API |
| `21-bulk-api` | Bulk API 2.0 over plain HTTP: an ingest job's lifecycle and CSV results, failed rows, a query job's CSV and headers, errors |
| `22-sales-objects` | Opportunity products (names and prices from the entry, `UnitPrice` vs `TotalPrice`, quantity and discount updates, inactive and other-price-book entries, the opportunity's price book, `Amount` and quantity, deletes, describe), contact roles (one primary), campaign members and campaign counts, contract end dates, orders (products, total, activation locks products) and assets |
| `23-org-metadata` | Standard fields (compound addresses, an opportunity's forecast category and fiscal period, a case's contact details), page / compact / approval / named layouts, list views with their describe, results and plans, query plans, tabs and theme, the version root's identity URL, `/limits`, and a rolled-back upsert's `created` |

A case can be marked `todo`: Salesforce behaviour localforce doesn't have
yet. Against localforce it runs but can't fail the suite (a node:test `todo`,
a pytest `xfail`, a Go skip); against a real org it's an ordinary case. When
the feature lands, the case starts passing: delete its `todo`. There are none
at the moment.

### How each SDK runs a case

Each runner uses its SDK's own API for an operation where it has one, and
its generic REST call where it doesn't. Every runner's header comment has the
details.

| Operation | jsforce | simple-salesforce | go-salesforce |
|---|---|---|---|
| create / update / delete | `sobject().create()` / `.update()` / `.destroy()` | `sf.<Type>.create()` / `.update()` / `.delete()` | `InsertOne` / `UpdateOne` / `DeleteOne` |
| retrieve | `sobject().retrieve()` | `sf.<Type>.get()` | `DoRequest` |
| upsert | `sobject().upsert()` | `sf.<Type>.upsert(raw_response=True)` | `DoRequest` (`UpsertOne` doesn't return `created`) |
| query | `query().run({ autoFetch })` | `query()` + `query_more()` | `Query` (+ `DoRequest` for `totalSize` and `queryAll`) |
| describe | `describe()` / `describeGlobal()` | `sf.<Type>.describe()` / `sf.describe()` | `DoRequest` |
| collections | `sobject().create([…])` etc. | `restful("composite/sobjects")` | `InsertCollection` etc. (`DoRequest` for `allOrNone` or mixed types) |
| composite | `requestPost("/composite")` | `restful("composite")` | `DoRequest` |
| request | `fetch` with the session token | the SDK's `requests` session | `net/http` with the session token |

## SDK-specific tests

SDK APIs that a case can't express are tested in each suite's own tests
(`jsforce.test.mjs`, `test_simple_salesforce.py`, `sdk_test.go`):

| SDK API | jsforce | simple-salesforce | go-salesforce |
|---|---|---|---|
| OAuth username-password login | `login` | `test_oauth_password_flow_with_consumer_key` | `TestLogin/username-password` |
| Client credentials | `login` (`fetch`) | `test_client_credentials_flow` | `TestLogin/client credentials` |
| Access token | spec (every runner) | `test_direct_session_and_instance_url` | `TestLogin/access token, validated` |
| SOAP login | `SOAP login finds the instance…` | `test_soap_login_with_security_token` | none (no SOAP login) |
| Authorization code (PKCE), the user picker, refresh tokens | `authorization code with PKCE…`, `the authorization page…` | none (no browser flow) | none (no browser flow) |
| JWT bearer | `the JWT bearer flow` | `test_jwt_bearer_flow` (and an unknown user) | `TestLogin/JWT bearer` |
| Bad logins | `invalid_grant`, `INVALID_LOGIN` fault | `…unknown_user_fails` (both flows) | `TestLogin/an unknown user…`, `…bad access token…` |
| Logout / revoke, and refreshing | `logout() revokes the session…` | `test_an_expired_session_is_renewed_by_logging_in_again` | `TestSessionRefresh` |
| Identity / userinfo | `identity()` | `test_oauth2_userinfo` | spec |
| Paging helpers | `queryMore`, `autoFetch` / `maxFetch`, record events, `scanAll` | `query_more`, `query_all_iter`, `query_all`, `include_deleted` | spec (`Query` follows pages) |
| Typed results | none | `parse_float=Decimal` | `TestQueryIntoStructs` |
| Query builders | `find()` / `count()` / `findOne()` / `include()` | none | `TestQueryStruct` (go-soql) |
| Single-record helpers | spec | return values, `get_by_custom_id`, upsert status codes | `TestSingleRecordsWithStructs` |
| Batching | `allowRecursive`, the 200-record limit | none (no Collections API) | `TestCollections` (450 records in 3 calls), `TestCompositeHelpers` |
| Errors | spec | exception classes and `.content` | `…errors carry Salesforce's code and fields` |
| Limits | `limitInfo`, `limits()` | `limits()`, `api_usage` | none |
| Describe | `describe$` cache, unique key prefixes | `metadata()`, `describe()` | spec |
| gzip | none | none | `TestCompression` |
| SOAP calls | `soap.convertLead()`, `soap.merge()`, `soap.undelete()` | none (no SOAP calls) | none (no SOAP calls) |
| UI metadata | `layouts()`, `compactLayouts()`, `approvalLayouts()`, `listviews()` / `listview().describe()` / `.results()` / `.explain()`, `query().explain()`, `tabs()`, `theme()` | spec | spec |
| Generic REST | `request()` | `restful()`, `apexecute()` | `DoRequest` |
| Search | `search()` | `search()`, `quick_search()`, errors | none (no search API) |
| Replication | `sobject().updated()` / `.deleted()` | `sf.<Type>.updated()` / `.deleted()` | none |
| Recent items | `recent()`, `sobject().recent()` | none | none |
| Tooling API | `tooling.query()`, `describeGlobal()`, `describe()` | `toolingexecute()` | none |
| Bulk API 2.0 ingest | `bulk2` (`bulk.test.mjs`): insert / update / upsert / delete / hardDelete, results, relationship columns, delimiters, aborting | `sf.bulk2` (`test_bulk.py`): insert, CSV files, update / upsert / delete / hardDelete, tab-delimited | `InsertBulk` etc. (`bulk_test.go`): results per row, several jobs (gzipped), CSV files |
| Bulk API 2.0 query | `bulk2.query()`: CSV, paging by locator, `queryAll` | `sf.bulk2.<Type>.query()` pages, `query_all`, `download` | `QueryBulkExport`, `QueryBulkIterator`, results headers |
| Bulk API 1.0 | `bulk.load()` / `bulk.query()`: CSV and XML jobs, job and batch info | `sf.bulk` (JSON jobs): insert, update, upsert, delete, hardDelete, query | none (go-salesforce only uses 2.0) |

A new SDK API gets a test in every suite whose SDK has it.

`tests/node/localforce.test.mjs` covers localforce's own API (the web UI's
backend): objects and fields made in the UI are visible to SDKs at once, the
request log, and SFDX export. `tests/node/ui-smoke.mjs` drives the web UI in
headless Chrome against a running server (`node ui-smoke.mjs <screenshot dir>`).

## SDK differences

These are differences between the SDKs, not localforce. The runners bridge
them; none needs a case skipped.

- **simple-salesforce** only builds `https://` URLs, so it uses localforce's
  HTTPS port and trusts its certificate (`<data-dir>/localforce-cert.pem`).
  Its logins only go to `https://{domain}.salesforce.com`, so the login tests
  mount a `requests` adapter that sends those URLs to localforce, which is how
  you'd point it at localforce too. `query_all()`'s `totalSize` is the number
  of records it fetched (0 for `COUNT()`), and it has no sObject Collections API.
- **go-salesforce** doesn't expose `totalSize`, always sends `allOrNone=false`
  in collections, gives every record of a call the same type, and doesn't
  return `created` from upserts. Its paging strips a hard-coded `/services/data/v63.0`
  from `nextRecordsUrl`, so the suites use API version 63.0. With only an
  access token it can't refresh a session, so an invalid token is a generic error.
- **jsforce** gives every record of a collections call the same type, and
  refuses more than 200 records unless `allowRecursive` is set.

## Running against a real org

This checks the cases against Salesforce itself: a failure means the case is
wrong about Salesforce, so fix the case first, then localforce. Use a scratch
org or a sandbox, never production. The suites create records, tagged with a
unique run tag, and delete them afterwards.

1. Deploy the fixture schema and give the test user access:

   ```sh
   cd tests/conformance/fixtures/sfdx
   sf org create scratch --definition-file config/project-scratch-def.json --alias lf-tests
   sf project deploy start --target-org lf-tests
   sf org assign permset --name Localforce_Tests --target-org lf-tests
   ```

   The scratch org definition turns on Orders and makes the user a Marketing
   User, which `22-sales-objects` needs for orders and campaigns. For a
   sandbox, do both in Setup.

2. Run any suite with the org's access token:

   ```sh
   eval "$(sf org display --target-org lf-tests --json | node -e '
     const r = JSON.parse(require("fs").readFileSync(0)).result;
     console.log(`export SF_INSTANCE_URL=${r.instanceUrl} SF_ACCESS_TOKEN=${r.accessToken}`)')"
   (cd tests/node && npm test)
   ```

   The login tests also need a connected app with the username-password flow
   enabled: `SF_LOGIN_URL`, `SF_USERNAME`, `SF_PASSWORD` (with the security token
   appended; the Go suite also takes it as `SF_SECURITY_TOKEN`), `SF_CLIENT_ID` and `SF_CLIENT_SECRET`. Without them those tests
   are skipped. Set `LF_SECOND_USERNAME` to another active user to run the
   cases that change a record's owner.

Tests that only make sense on localforce are skipped, and `todo` cases become
ordinary cases. A real org works in the background in places localforce
doesn't: it indexes records for search and runs bulk jobs asynchronously. The
cases wait for it with `settle` steps, which only sleep against a real org,
and the SDK tests wait the same way. The hardDelete tests are skipped on a real
org, which doesn't grant the Bulk API Hard Delete permission by default.

## Upstream test suites

`tests/upstream/jsforce.mjs` runs jsforce's own integration tests, the ones
its maintainers run against a real scratch org, against localforce:

```sh
cargo build
node tests/upstream/jsforce.mjs                      # or one file: test/query.test.ts
```

It fetches jsforce at a pinned commit into `tests/upstream/.cache`, builds it,
converts its test package's custom objects to SFDX source, and loads what
jsforce's org setup loads (25,005 `BigTable__c` rows) plus sample data like a
Developer Edition org's. It runs the test files for the APIs localforce
emulates. The tests that need what localforce doesn't have (approval
processes, Apex, Tooling API writes, the `ActivityHistories` relationship) are
listed as expected failures. The run fails on any other failure, and when an
expected failure starts passing.

At the pinned commit: 198 pass, 19 are expected failures, and 11 are skipped
by jsforce itself.

simple-salesforce's and go-salesforce's own tests use mocked responses, not a
real org, so they have nothing to check localforce against.

## Writing a case

- Add it to the `tests/conformance/cases` file for its area, and run it
  through all three suites.
- Assert only behaviour you've confirmed on a real org or in Salesforce's
  documentation. Where Salesforce's behaviour is ambiguous, assert only the
  part that isn't, or accept several error codes.
- Assert error codes and the fields they name. Match message text only where
  Salesforce's wording is stable, such as `No such column 'X'`.
- Put `{tag}` in every name or key you create, and filter queries by it.

## Not yet covered

- Streaming (PushTopics, Platform Events, Change Data Capture), the Metadata
  API (beyond `readMetadata` of fields), approval processes and Apex (Apex
  REST, `executeAnonymous`): localforce doesn't have them.
- Quotes, and the `ActivityHistories` / `OpenActivities` relationships.
- Other SDKs: Java has no maintained REST client (Salesforce's own WSC speaks
  SOAP), and the .NET and Ruby clients (Force.com Toolkit, restforce) aren't
  tested yet.
- Each suite tests one SDK version.
