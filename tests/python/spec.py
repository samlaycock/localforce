"""The SDK-independent half of the conformance runner: loading the spec,
placeholders, and matching expected values. tests/node/lib/spec.mjs and
tests/go implement the same rules; see tests/conformance/README.md."""

import json
import re
import secrets
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

SPEC_DIR = Path(__file__).resolve().parents[1] / "conformance" / "cases"


class Missing:
    """A key that isn't there (JavaScript's undefined)."""

    def __repr__(self):
        return "undefined"


MISSING = Missing()


def load_spec():
    """Every spec file, in name order: `{file, about, setup, cases}`."""
    files = []
    for path in sorted(SPEC_DIR.glob("*.json")):
        spec = {"file": path.stem, "setup": []}
        spec.update(json.loads(path.read_text()))
        files.append(spec)
    return files


def _base36(n: int) -> str:
    digits = "0123456789abcdefghijklmnopqrstuvwxyz"
    out = ""
    while n:
        n, r = divmod(n, 36)
        out = digits[r] + out
    return out or "0"


def new_tag() -> str:
    """Always 14 characters: "lf", 6 of the time in base 36, 6 random hex digits."""
    return f"lf{_base36(int(time.time() * 1000))[-6:].rjust(6, '0')}{secrets.token_hex(3)}"


PLACEHOLDER = re.compile(r"\{([a-zA-Z_][a-zA-Z0-9_]*)(?::([-+]?\d+))?\}")
WHOLE = re.compile(r"^\{([a-zA-Z_][a-zA-Z0-9_]*)\}$")


def utc_date(days: int) -> str:
    return (datetime.now(timezone.utc) + timedelta(days=days)).date().isoformat()


def now_iso() -> str:
    now = datetime.now(timezone.utc)
    return now.strftime("%Y-%m-%dT%H:%M:%S.") + f"{now.microsecond // 1000:03d}Z"


def substitute(value, ctx):
    """Replace placeholders in every string in `value`. `ctx` has `tag`,
    `user`, `user2`, `version` and `saved` (a dict of names to values)."""
    if isinstance(value, str):
        whole = WHOLE.match(value)
        if whole and whole.group(1) in ctx["saved"]:
            return ctx["saved"][whole.group(1)]

        def resolve(m):
            name, arg = m.group(1), m.group(2)
            if name == "tag":
                return ctx["tag"]
            if name == "date":
                return utc_date(int(arg or 0))
            if name == "year":
                return utc_date(int(arg or 0))[:4]
            if name == "now":
                return now_iso()
            if name == "user":
                return ctx["user"]
            if name == "version":
                return ctx["version"]
            if name == "user2":
                if not ctx.get("user2"):
                    raise ValueError('{user2} needs a second user: add "needs": ["user2"] to the case')
                return ctx["user2"]
            if name in ctx["saved"]:
                v = ctx["saved"][name]
                v = js_string(v)
                return v[:15] if arg == "15" else v
            # Names the step itself saves are filled in when its expectations are checked.
            if name in ctx.get("later", ()):
                return m.group(0)
            raise ValueError(f"unknown placeholder {m.group(0)}")

        return PLACEHOLDER.sub(resolve, value)
    if isinstance(value, list):
        return [substitute(v, ctx) for v in value]
    if isinstance(value, dict):
        return {k: substitute(v, ctx) for k, v in value.items()}
    return value


def js_string(v) -> str:
    """String(v), as JavaScript would write it."""
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, float) and v.is_integer():
        return str(int(v))
    return str(v)


def dig(value, path: str):
    """Follow a dotted path ("Account.Name", "1.body.id") into a value."""
    for key in path.split("."):
        if isinstance(value, dict):
            value = value.get(key, MISSING)
        elif isinstance(value, list) and key.isdigit() and int(key) < len(value):
            value = value[int(key)]
        else:
            return MISSING
    return value


def show(v) -> str:
    if v is MISSING:
        return "undefined"
    try:
        return json.dumps(v, default=str)
    except (TypeError, ValueError):
        return repr(v)


def fail(where: str, msg: str):
    raise AssertionError(f"{where or 'value'}: {msg}")


SF_DATETIME = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}\+0000$")
ID18 = re.compile(r"^[a-zA-Z0-9]{18}$")


def is_number(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def records_of(actual):
    """A child relationship in a query record ({totalSize, done, records}) as its list."""
    if isinstance(actual, dict) and isinstance(actual.get("records"), list):
        return actual["records"]
    return actual


def match(expected, actual, where: str = "", exact: bool = False):
    """Assert that `actual` matches `expected` (see "Matching" in the spec's
    README). `exact` makes objects need the same keys (apart from
    attributes), as query records do."""
    if isinstance(expected, str) and expected.startswith("$"):
        if expected == "$id":
            if not (isinstance(actual, str) and ID18.match(actual)):
                fail(where, f"expected an 18-character id, got {show(actual)}")
            return
        if expected in ("$datetime", "$recent"):
            if not (isinstance(actual, str) and SF_DATETIME.match(actual)):
                fail(where, f"expected a Salesforce datetime, got {show(actual)}")
            if expected == "$recent":
                when = datetime.strptime(actual, "%Y-%m-%dT%H:%M:%S.%f%z")
                if abs((when - datetime.now(timezone.utc)).total_seconds()) > 600:
                    fail(where, f"expected a time within 10 minutes of now, got {actual}")
            return
        if expected == "$any":
            if actual is MISSING:
                fail(where, "expected a value")
            return
        if expected == "$notnull":
            if actual is None or actual is MISSING:
                fail(where, f"expected a value other than null, got {show(actual)}")
            return
        if expected == "$absent":
            if actual is not MISSING:
                fail(where, f"expected no value, got {show(actual)}")
            return
    if isinstance(expected, dict):
        keys = list(expected)
        if len(keys) == 1 and keys[0].startswith("$"):
            return matcher(keys[0], expected[keys[0]], actual, where, exact)
        if not isinstance(actual, dict):
            fail(where, f"expected an object, got {show(actual)}")
        for k in keys:
            sub = f"{where}.{k}" if where else k
            if expected[k] == "$absent":
                if k in actual:
                    fail(sub, f"expected no such key, got {show(actual[k])}")
                continue
            if k not in actual:
                fail(sub, f"missing (have {', '.join(actual)})")
            match(expected[k], actual[k], sub, exact and k != "attributes")
        if exact:
            extra = [k for k in actual if k != "attributes" and k not in expected]
            if extra:
                fail(where, f"unexpected keys {', '.join(extra)}")
        return
    if isinstance(expected, list):
        items = records_of(actual)
        if not isinstance(items, list):
            fail(where, f"expected an array, got {show(actual)}")
        if len(items) != len(expected):
            fail(where, f"expected {len(expected)} items, got {len(items)}: {show(items)}")
        for i, (e, a) in enumerate(zip(expected, items)):
            match(e, a, f"{where}[{i}]", exact)
        return
    if is_number(expected):
        if not is_number(actual) or abs(expected - actual) > 1e-9:
            fail(where, f"expected {show(expected)}, got {show(actual)}")
        return
    if isinstance(expected, bool):
        if not (isinstance(actual, bool) and actual == expected):
            fail(where, f"expected {show(expected)}, got {show(actual)}")
        return
    if expected is None:
        if actual is not None:
            fail(where, f"expected null, got {show(actual)}")
        return
    if type(actual) is not type(expected) or actual != expected:
        fail(where, f"expected {show(expected)}, got {show(actual)}")


def _compare(actual, arg, op) -> bool:
    if actual is None or actual is MISSING:
        return False
    if is_number(actual) and is_number(arg) or isinstance(actual, str) and isinstance(arg, str):
        return actual > arg if op == "gt" else actual < arg
    return False


def matcher(name, arg, actual, where, exact):
    if name == "$regex":
        if not (isinstance(actual, str) and re.search(arg, actual)):
            fail(where, f"expected a match for /{arg}/, got {show(actual)}")
        return
    if name == "$approx":
        if not is_number(actual) or abs(actual - arg) > 0.01:
            fail(where, f"expected about {arg}, got {show(actual)}")
        return
    if name in ("$gt", "$lt"):
        if not _compare(actual, arg, name[1:]):
            fail(where, f"expected {'more' if name == '$gt' else 'less'} than {show(arg)}, got {show(actual)}")
        return
    if name == "$length":
        items = records_of(actual)
        if not isinstance(items, list) or len(items) != arg:
            fail(where, f"expected {arg} items, got {show(actual)}")
        return
    if name in ("$contains", "$unordered"):
        items = records_of(actual)
        if not isinstance(items, list):
            fail(where, f"expected an array, got {show(actual)}")
        if name == "$unordered" and len(items) != len(arg):
            fail(where, f"expected {len(arg)} items, got {len(items)}: {show(items)}")
        used = set()
        for e in arg:
            found = next((j for j, a in enumerate(items) if j not in used and matches(e, a, exact)), None)
            if found is None:
                fail(where, f"no item matches {show(e)} in {show(items)}")
            used.add(found)
        return
    if name == "$excludes":
        items = records_of(actual)
        if not isinstance(items, list):
            fail(where, f"expected an array, got {show(actual)}")
        for e in arg:
            if any(matches(e, a, exact) for a in items):
                fail(where, f"expected no item matching {show(e)} in {show(items)}")
        return
    fail(where, f"unknown matcher {name}")


def matches(expected, actual, exact=False) -> bool:
    try:
        match(expected, actual, "", exact)
        return True
    except AssertionError:
        return False


def match_error(expected, actual: dict, where: str = "error"):
    """Check a normalised error `{code, fields, message}` against a step's
    `error`: a code, a list of codes, or `{code, fields, message}`."""
    exp = {"code": expected} if isinstance(expected, (str, list)) else expected
    codes = exp["code"] if isinstance(exp["code"], list) else [exp["code"]]
    if actual.get("code") not in codes:
        fail(where, f"expected {' or '.join(codes)}, got {actual.get('code')}: {actual.get('message')}")
    if exp.get("fields") is not None:
        if sorted(actual.get("fields") or []) != sorted(exp["fields"]):
            fail(where, f"expected fields {show(exp['fields'])}, got {show(actual.get('fields'))}")
    if exp.get("message") and exp["message"] not in str(actual.get("message")):
        fail(where, f"expected the message to contain {show(exp['message'])}, got {show(actual.get('message'))}")


def match_results(expected, actual):
    """Check save results (`{success, id, created, errors}`, one per record)
    against `results`: "ok", `{created}` or `{error, fields}`."""
    if not isinstance(actual, list) or len(actual) != len(expected):
        fail("results", f"expected {len(expected)} results, got {show(actual)}")
    for i, (e, r) in enumerate(zip(expected, actual)):
        where = f"results[{i}]"
        if e == "ok" or (isinstance(e, dict) and "error" not in e):
            if not r.get("success"):
                fail(where, f"expected success, got {show(r.get('errors'))}")
            if isinstance(e, dict) and "created" in e and r.get("created") != e["created"]:
                fail(where, f"expected created {e['created']}, got {show(r.get('created'))}")
            continue
        if r.get("success"):
            fail(where, f"expected {show(e['error'])}, got success")
        err = (r.get("errors") or [{}])[0]
        match_error(
            {"code": e["error"], "fields": e.get("fields"), "message": e.get("message")},
            {"code": err.get("statusCode"), "fields": err.get("fields"), "message": err.get("message")},
            where,
        )

