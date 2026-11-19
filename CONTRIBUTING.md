# Contributing to localforce

Thanks for helping out. Bug reports, Salesforce behaviour that localforce gets
wrong, and new features are all welcome. For anything large, please open an
issue first so we can agree on the approach.

The most useful bug reports include the request, the response localforce
gives, and the response a real org gives (or the documentation page that
describes it).

## Layout

| Module | Role |
|---|---|
| `routes.rs`, `oauth.rs` | HTTP routes: identity, the SOAP login call and the REST API; OAuth 2.0's grants and the authorization endpoint's user picker |
| `rest.rs` | The REST API under `/services/data/vXX.X/`, through one dispatcher so `/composite` runs its subrequests through the same code |
| `schema.rs`, `sfdx.rs`, `schema_admin.rs` | The org's metadata: the built-in objects, layered with `localforce.toml`, SFDX metadata and objects made in the web UI |
| `store.rs`, `ids.rs` | Record storage (one SQLite table per sObject) and the DML rules Salesforce applies on the way in; record ids |
| `soql/`, `sosl.rs` | SOQL, parsed by hand (so errors carry Salesforce's row and column) and compiled to SQLite; SOSL |
| `formula.rs` | Formula fields, roll-up summaries and validation rules, compiled to SQLite expressions |
| `sales.rs` | The sales objects' own logic: opportunity products, price books, contact roles, campaigns, contracts, orders and assets |
| `bulk.rs`, `tooling.rs`, `soap.rs` | The Bulk APIs 2.0 and 1.0, the read-only Tooling API, and the Partner SOAP calls REST has no equivalent for |
| `describe.rs`, `layouts.rs`, `listviews.rs`, `tabs.rs`, `explain.rs` | Describes, page layouts, list views, tabs and query plans |
| `ui.rs`, `inspector.rs` | The web UI's JSON API and the API log. The page itself is `ui/` (plain HTML, CSS and JavaScript, embedded in the binary) |

[DESIGN.md](DESIGN.md) covers the architecture, the hard parts and the
roadmap.

## Development

You need a recent stable Rust toolchain. The SDK suites also need Node.js,
Python with [uv](https://docs.astral.sh/uv/) and Go, and Node.js is needed for
changesets.

```sh
cargo run                                    # HTTP on http://localhost:9600, HTTPS on https://localhost:9643
cargo run -- --metadata ../my-app/force-app  # with a project's custom objects and fields
LOCALFORCE_UI_DIR=ui cargo run               # serve ui/ from disk: edit and reload
docker build -t localforce .                 # the release image
scripts/docker-smoke.sh localforce
```

The web UI's files are embedded at build time. While working on them, set
`LOCALFORCE_UI_DIR` so a browser reload picks up changes without a rebuild.
The page has no build step or dependencies.

## Testing

```sh
cargo test                                   # Rust tests
cargo build                                  # the SDK suites start target/debug/localforce
(cd tests/node && npm ci && npm test)        # jsforce
(cd tests/python && uv run pytest)           # simple-salesforce
(cd tests/go && go test -count=1 ./...)      # go-salesforce (-count=1: results depend on the server binary)
node tests/upstream/jsforce.mjs              # jsforce's own integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Set `LOCALFORCE_BIN` to test a different binary. CI runs `cargo fmt --check`,
clippy with `-D warnings`, `cargo test`, `cargo audit`, the three SDK suites
and jsforce's own integration tests on every pull request, and a Docker build
and smoke test on release pull requests.

The SDK suites share a conformance spec (`tests/conformance/cases/*.json`)
that each suite plays through its SDK, so they get the same coverage. The
cases describe Salesforce's behaviour, not localforce's, so they also run
against a real org. [`tests/README.md`](tests/README.md) describes the spec,
each SDK's own tests and how to run them against an org. API behaviour changes
should add a case there.

## Changesets

Every pull request needs a changeset: a short Markdown file under `.changeset/`
that says what changed and whether it's a `patch`, `minor` or `major` change.
It becomes the changelog entry.

```sh
npm ci
npm run changeset                # describe a change users will notice
npm run changeset -- --empty     # for CI, tests, docs and other invisible changes
```

CI fails if a pull request has no changeset.

## Releases

Releases are managed with [changesets](https://github.com/changesets/changesets).
`package.json` holds the version, and `npm run version` copies it into
`Cargo.toml` and `Cargo.lock`.

1. Changesets land on `main` with their pull requests.
2. The release workflow keeps a "Release <version>" pull request open. It
   applies the pending changesets: the version bump and `CHANGELOG.md`. CI runs
   on it, including a Docker build and smoke test.
3. Merging that pull request does the release:
   - builds the image natively for amd64 and arm64;
   - pushes `ghcr.io/samlaycock/localforce:<version>`, `:<major>.<minor>` (and
     `:<major>` from 1.0) and `:latest`;
   - tags `v<version>` and creates the GitHub release from the changelog.

## License

By contributing, you agree that your contributions will be dual licensed under
the MIT and Apache-2.0 licenses, as described in the
[README](README.md#license).
