# SDK conformance spec

`cases/*.json` is shared by every SDK suite (`tests/node`, `tests/python`,
`tests/go`). Each suite has a runner that plays every case through its SDK and
compares the results in a canonical form. So every SDK gets the same coverage,
and a new case covers every SDK at once.

The cases describe **Salesforce's** behaviour, not localforce's: the same
files run against a real org (see `tests/README.md`). `fixtures/` holds the
schema they use: an SFDX project (`fixtures/sfdx`) that localforce loads with
`--metadata` and that you deploy to a scratch org, and the localforce users
(`fixtures/localforce.toml`).

SDK APIs that a case can't express (login flows, paging helpers, query
builders, batching, retries) are tested in each suite's own tests; see
`tests/README.md` for the checklist.

## Files

```jsonc
{
  "about": "What these cases cover.",
  "setup": [ /* steps run once, before the file's cases */ ],
  "cases": [
    {
      "name": "create/returns an id",   // unique in the file; runners name tests "<file>/<name>"
      "skip": { "python": "why" },      // optional: SDKs (node, python, go) that can't run it, and why
      "todo": "SOSL",                   // optional: localforce doesn't do this yet (see below)
      "needs": ["user2"],               // optional: skip unless the target has these (see Values)
      "steps": [ /* run in order */ ]
    }
  ]
}
```

A file's cases run in order, after its `setup`, and share its tag and saved
names. Records a step creates are deleted after its case (or, for `setup`,
after the file), newest first, so cases don't see each other's data. Write
cases so they don't depend on other cases having run.

**`todo`** marks Salesforce behaviour localforce doesn't emulate yet. Against
localforce the case still runs, but a failure is reported as a todo (a node:test
`todo`, a pytest `xfail`, a Go skip) rather than failing the suite. Against a
real org it's an ordinary case. When the feature lands, delete the `todo`.

## Steps

Every step has exactly one operation. Each SDK uses its own API for the
operation where it has one, and its generic REST call where it doesn't; the
table in `tests/README.md` says which.

| Operation | Fields | Expectations |
|---|---|---|
| `"create": "Account"` | `record` | |
| `"retrieve": "Account"` | `id`, `fields` (optional list) | `record` (subset) |
| `"update": "Account"` | `id`, `record` | |
| `"upsert": "Account"` | `field`, `value`, `record` (without the external id) | `created` |
| `"delete": "Account"` | `id` | |
| `"query": "SELECT …"` | `all` (queryAll: include deleted records) | `records`, `total`, `pluck` + `values` / `contains` / `excludes`, `unordered` |
| `"describe": "Account"` | | `object` (subset), `fields`, `child_relationships` |
| `"describe_global": true` | | `object` (subset), `sobjects` |
| `"create_many": "Account"` | `records` (or `count` + `template`), `all_or_none` | `results` |
| `"update_many": "Account"` | `records` (each with `Id`), `all_or_none` | `results` |
| `"upsert_many": "Account"` | `field`, `records`, `all_or_none` | `results` |
| `"delete_many": true` | `ids`, `all_or_none` | `results` |
| `"retrieve_many": "Account"` | `ids`, `fields` | `records` (subsets, or `null` for an id that wasn't found) |
| `"composite": {…}` | the `/composite` request body | `responses` (subsets of `compositeResponse`, in order) |
| `"request": "GET"` | `path`, `body` or `raw_body`, `headers`, `auth` | `status`, `response`, `response_headers` |
| `"sleep": 1100` | milliseconds | |
| `"settle": 5000` | milliseconds, slept only against a real org (e.g. while it indexes records for search) | |

Any step can also have:

- **`as`**: names for the ids the step created: a string for `create` and
  `upsert`, a list for `create_many` and `upsert_many`. Use them later as `{name}`.
- **`save`**: `{ "name": "path" }` saves values from the result: a field of
  the retrieved record, of a query's first record (`"Name"`, `"Account.Name"`),
  of the response body of a `request`, or of a `composite` response
  (`"1.body.id"`). Saving happens before the expectations are checked, so
  they can use the saved values.
- **`error`**: the step must fail with this Salesforce error (see Errors).
  Without `error`, the step must succeed.

Details:

- **`query`** fetches every page through the SDK (following `nextRecordsUrl`).
  `records` must match exactly: each record has the selected fields and no
  others. `total` is `totalSize`. With `pluck: "Name"`, `values` lists that
  field of each record in order, and `contains` / `excludes` list values that
  must or mustn't appear. `"unordered": true` ignores the order of `records`
  and `values`.
- **`create_many`** and **`update_many`** records can carry their own type
  in `"attributes": {"type": "Contact"}`. Give `true` instead of an object
  name to use only those types. `delete_many` doesn't need a type, so it's
  always `true`.
- **`count` + `template`** generate `count` records for `create_many` from
  `template`, in which `{i}` is the record's number (`0000`, `0001`, …).
  They're sent 200 at a time, the most one call takes, and `results` (if
  given) covers them all.
- **`results`** has one entry per record, in order: `"ok"`, `{"created": true}`
  (upserts), or an error, e.g. `{"error": "REQUIRED_FIELD_MISSING", "fields": ["Name"]}`.
- **`describe`**: `fields` maps field names to a subset of the field's
  describe, or `"$absent"`. `child_relationships` maps relationship names to
  a subset of the child relationship. `sobjects` (global describe) maps object
  names to a subset of their entry.
- **`request`** is a plain HTTP call with the SDK's session token, for the
  API's own behaviour (status codes, headers, error bodies) rather than an
  SDK's. A `path` starting with `/services` is relative to the instance URL;
  anything else to `/services/data/v{version}`. `body` is sent as JSON,
  `raw_body` as is. `"auth": false` sends no token. `status` is checked
  exactly and `response` (the response body) as a subset;
  `response_headers` maps header names to a regular expression (or
  `"$absent"`). An `error` is read from the response body, and doesn't
  replace checking `status`.

## Values

Strings in a step can contain:

| Placeholder | Value |
|---|---|
| `{tag}` | A tag unique to this run of the file: `lf` and 12 lowercase letters and digits, e.g. `lfmhx3k2a9f1c2` (always 14 characters). Put it in names, and filter queries by it, so cases don't see other runs' data (or an org's). |
| `{name}` | A value saved with `as` or `save`. A string that's only `{name}` keeps the saved value's type. |
| `{name:15}` | The 15-character form of a saved id. |
| `{date:N}` | The UTC date N days from today: `{date:0}`, `{date:-10}`. |
| `{year:N}` | The year of `{date:N}`. |
| `{now}` | The current UTC time: `2026-03-01T09:30:00.000Z`. |
| `{user}` | The session user's Id. |
| `{user2}` | Another active user's Id. Cases using it need `"needs": ["user2"]`. |
| `{version}` | The API version, e.g. `63.0`. |
| `{i}` | In a `template`, the record's number. |

An unknown placeholder is an error, so typos don't go unnoticed.

## Matching

Expected values match actual values like this:

- Strings, booleans and `null` must be equal; numbers must be numerically
  equal (`1500` matches `1500.0`).
- Objects match if each expected key matches. In query `records` (and parent
  records within them) the keys must also be the same, apart from
  `attributes`, which is only checked if expected, and then as a subset.
- Arrays must have the same length and match element by element. A child
  relationship in a query record matches an array of its records (or `null`
  when it has none).
- Matchers:

| Matcher | Matches |
|---|---|
| `"$id"` | An 18-character record id. |
| `"$datetime"` | A datetime in Salesforce's format, `2026-03-01T09:30:00.000+0000`. |
| `"$recent"` | A datetime in Salesforce's format within 10 minutes of now. |
| `"$any"` | Any value, but the key must be present. |
| `"$notnull"` | Anything but `null`. |
| `"$absent"` | The key must be missing. |
| `{"$regex": "^TI-\\d{4}$"}` | A string matching the regular expression (it isn't anchored unless it says so). |
| `{"$approx": 333.33}` | A number within 0.01. |
| `{"$gt": "{a}"}`, `{"$lt": 5}` | A number or string greater or less than the value. |
| `{"$length": 3}` | An array of this length. |
| `{"$contains": [ … ]}` | An array with elements matching each of these, in any order. |
| `{"$unordered": [ … ]}` | An array matching these elements in any order. |
| `{"$excludes": [ … ]}` | An array with no element matching any of these. |

## Errors

`"error"` is a Salesforce error code, a list of codes any of which is
accepted, or an object:

```json
{ "code": "INVALID_FIELD", "fields": ["Nope__c"], "message": "No such column 'Nope__c'" }
```

`fields` must equal the error's fields (in any order), and `message` must be
a substring of its message. Runners read these from each SDK's error type.
In `results`, `{"error": …}` takes the same forms.
