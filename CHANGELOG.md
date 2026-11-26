# localforce

## 0.2.0

### Minor Changes

- [#3](https://github.com/samlaycock/localforce/pull/3) [`0151ddb`](https://github.com/samlaycock/localforce/commit/0151ddbc464288373d3310bcd960a440da2de1b9) Thanks [@samlaycock](https://github.com/samlaycock)! - The web UI is rebuilt as a React app with shadcn/ui components. It works as before, with a few changes:
  
  - Each view has its own address under `/__localforce/ui/`, such as `/__localforce/ui/data/Account/<id>` for a record and `/__localforce/ui/log/<n>` for a logged request. Old `#/data/…` links still work.
  - A table's search, recycle bin, sort and page are kept in the URL, so a filtered list can be shared or bookmarked.
  - The objects list, the record editor and the API log's detail can be resized, and there's a light, dark and system theme switch.
  - `LOCALFORCE_UI_DIR` is gone: debug builds read the built UI from disk, and `bun run dev` in `ui/` serves it with hot reload.

## 0.1.0

### Minor Changes

- [`9091cb3`](https://github.com/samlaycock/localforce/commit/9091cb38357d93c10c7d43672db4c2da9ddf1467) Thanks [@samlaycock](https://github.com/samlaycock)! - First release of localforce, a local Salesforce emulator for offline development and testing, published as a Docker image (`ghcr.io/samlaycock/localforce`) for linux/amd64 and linux/arm64. It speaks Salesforce's REST API and OAuth endpoints (over HTTP, and HTTPS with a generated certificate), so jsforce, simple-salesforce and go-salesforce connect to it unchanged: sObject CRUD, upsert by external id, describe, SOQL (relationship paths, subqueries, aggregates, date literals) with query paging, SOSL, sObject Collections, `/composite` (with tree, batch and graph), the Bulk APIs 2.0 and 1.0, a read-only Tooling API and the Partner SOAP calls REST has no equivalent for. Errors use Salesforce's codes and messages. The common sales and service objects are built in with their logic, and custom objects and fields come from an SFDX project (`--metadata`), `localforce.toml` or the web UI, with formula fields, roll-up summaries and validation rules evaluated. Every OAuth grant works, including authorization code with PKCE and the JWT bearer flow, with a user picker instead of a login form. A web UI browses and edits records, defines objects and fields, runs SOQL and logs every API call. The org is kept in memory, or in `--data-dir` between restarts.
