"""Runs the shared conformance spec (tests/conformance) through simple-salesforce.

How each operation maps to simple-salesforce:
- create / update / delete: SFType.create(), .update() and .delete()
  (`sf.Account.create(...)`).
- retrieve: SFType.get(). It has no `fields` argument, so `?fields=` goes on
  the end of the id (`get("001…?fields=Name,Phone")`).
- upsert: SFType.upsert("Field/value", data, raw_response=True). Without
  raw_response it returns only the status code; 201 means created.
- query: sf.query() for the first page (the only place totalSize is exact:
  query_all() counts the records it fetched, so a COUNT() query reads 0) and
  sf.query_more(nextRecordsUrl, identifier_is_url=True) for the rest.
  queryAll is `include_deleted=True`.
- describe / describe_global: SFType.describe() and sf.describe().
- create_many / update_many / upsert_many / delete_many / retrieve_many:
  simple-salesforce has no sObject Collections API (its bulk handlers use the
  Bulk APIs), so these use sf.restful("composite/sobjects…"). Without
  `fields`, retrieve_many asks describe for every field, as jsforce does: the
  API needs a field list.
- composite: sf.restful("composite", method="POST").
- request: the SDK's requests session, with its token.
"""

import json
import time
from urllib.parse import quote

import pytest
import requests
from requests.structures import CaseInsensitiveDict
from simple_salesforce.exceptions import SalesforceError

from conftest import API_VERSION
from spec import MISSING, dig, load_spec, match, match_error, match_results, new_tag, substitute

SPECS = load_spec()
CASES = [
    pytest.param(spec, case, id=f"{spec['file']}/{case['name']}") for spec in SPECS for case in spec["cases"]
]


def normalise_error(err: SalesforceError) -> dict:
    """A simple-salesforce error as `{code, fields, message}`."""
    content = err.content
    if isinstance(content, (bytes, str)):
        try:
            content = json.loads(content)
        except ValueError:
            return {"code": None, "fields": [], "message": str(content)}
    if isinstance(content, list) and content and isinstance(content[0], dict):
        first = content[0]
        return {"code": first.get("errorCode"), "fields": first.get("fields") or [], "message": first.get("message")}
    if isinstance(content, dict):
        return {"code": content.get("error"), "fields": [], "message": content.get("error_description")}
    return {"code": None, "fields": [], "message": str(content)}


def save_results(res) -> list:
    """Collections results as `{success, id, created, errors}` dicts."""
    return [{**r, "errors": r.get("errors") or []} for r in res]


def typed(records, sobject):
    """Give records `attributes.type`, which sObject Collections needs on every record."""
    if sobject is True:
        return records
    return [{"attributes": {"type": sobject}, **{k: v for k, v in r.items() if k != "attributes"}} for r in records]


class Runner:
    def __init__(self, sf, target):
        self.sf = sf
        self.target = target

    def sobject(self, name):
        return getattr(self.sf, name)

    def raw(self, method, path, body=MISSING, raw_body=MISSING, headers=None, auth=True):
        """A plain HTTP call with the session's token; returns (status, headers, body)."""
        if path.startswith("/services"):
            url = self.sf.base_url.split("/services/")[0] + path
        else:
            url = self.sf.base_url.rstrip("/") + path
        h = CaseInsensitiveDict()
        if auth:
            h["Authorization"] = f"Bearer {self.sf.session_id}"
        data = None
        if raw_body is not MISSING:
            h["Content-Type"] = "application/json"
            data = raw_body
        elif body is not MISSING:
            h["Content-Type"] = "application/json"
            data = json.dumps(body)
        for k, v in (headers or {}).items():
            h[k] = v
        res = self.sf.session.request(method, url, headers=dict(h), data=data)
        try:
            parsed = res.json() if res.content else None
        except ValueError:
            parsed = res.text
        return res.status_code, res.headers, parsed

    def operate(self, step) -> dict:
        """Run one operation; returns `{value, results, created, status, headers}`."""
        sf = self.sf
        all_or_none = bool(step.get("all_or_none"))
        if "create" in step:
            r = self.sobject(step["create"]).create(step["record"])
            return {"value": r, "created": [r["id"]]}
        if "retrieve" in step:
            rid = step["id"]
            if step.get("fields"):
                rid = f"{rid}?fields={','.join(step['fields'])}"
            return {"value": self.sobject(step["retrieve"]).get(rid)}
        if "update" in step:
            self.sobject(step["update"]).update(step["id"], step["record"])
            return {"value": {"id": step["id"], "success": True}}
        if "upsert" in step:
            res = self.sobject(step["upsert"]).upsert(
                f"{step['field']}/{quote(str(step['value']), safe='')}", step["record"], raw_response=True
            )
            body = res.json() if res.content else {}
            created = res.status_code == 201
            value = {"id": body.get("id"), "success": True, "created": created}
            return {"value": value, "created": [value["id"]] if created else []}
        if "delete" in step:
            self.sobject(step["delete"]).delete(step["id"])
            return {"value": {"id": step["id"], "success": True}}
        if "query" in step:
            include_deleted = bool(step.get("all"))
            res = sf.query(step["query"], include_deleted=include_deleted)
            total, records = res["totalSize"], list(res["records"])
            while not res["done"]:
                res = sf.query_more(res["nextRecordsUrl"], identifier_is_url=True)
                records.extend(res["records"])
            return {"value": {"totalSize": total, "records": records}}
        if "describe" in step:
            return {"value": self.sobject(step["describe"]).describe()}
        if "describe_global" in step:
            return {"value": sf.describe()}
        if "create_many" in step:
            out = []
            records = typed(step["records"], step["create_many"])
            for i in range(0, len(records), 200):
                res = sf.restful(
                    "composite/sobjects", method="POST", json={"allOrNone": all_or_none, "records": records[i : i + 200]}
                )
                out.extend(save_results(res))
            return {"value": out, "results": out, "created": [r["id"] for r in out if r.get("success")]}
        if "update_many" in step:
            res = sf.restful(
                "composite/sobjects",
                method="PATCH",
                json={"allOrNone": all_or_none, "records": typed(step["records"], step["update_many"])},
            )
            out = save_results(res)
            return {"value": out, "results": out}
        if "upsert_many" in step:
            res = sf.restful(
                f"composite/sobjects/{step['upsert_many']}/{step['field']}",
                method="PATCH",
                json={"allOrNone": all_or_none, "records": typed(step["records"], step["upsert_many"])},
            )
            out = save_results(res)
            return {"value": out, "results": out, "created": [r["id"] for r in out if r.get("success") and r.get("created")]}
        if "delete_many" in step:
            res = sf.restful(
                "composite/sobjects",
                method="DELETE",
                params={"ids": ",".join(step["ids"]), "allOrNone": "true" if all_or_none else "false"},
            )
            out = save_results(res)
            return {"value": out, "results": out}
        if "retrieve_many" in step:
            fields = step.get("fields") or [f["name"] for f in self.sobject(step["retrieve_many"]).describe()["fields"]]
            res = sf.restful(f"composite/sobjects/{step['retrieve_many']}", method="POST", json={"ids": step["ids"], "fields": fields})
            return {"value": res}
        if "composite" in step:
            res = sf.restful("composite", method="POST", json=step["composite"])
            responses = res["compositeResponse"]
            created = [
                r["body"]["id"]
                for r in responses
                if r.get("httpStatusCode") == 201 and isinstance(r.get("body"), dict) and r["body"].get("id")
            ]
            return {"value": responses, "created": created}
        if "request" in step:
            status, headers, body = self.raw(
                step["request"],
                step["path"],
                body=step.get("body", MISSING),
                raw_body=step.get("raw_body", MISSING),
                headers=step.get("headers"),
                auth=step.get("auth", True) is not False,
            )
            created = (
                [body["id"]]
                if step["request"] == "POST" and status == 201 and isinstance(body, dict) and body.get("id")
                else []
            )
            return {"value": body, "status": status, "headers": headers, "created": created}
        if "sleep" in step:
            time.sleep(step["sleep"] / 1000)
            return {"value": None}
        if "settle" in step:
            # Only a real org needs time to settle (e.g. to index records for search).
            if not self.target.localforce:
                time.sleep(step["settle"] / 1000)
            return {"value": None}
        raise ValueError(f"no operation in step {json.dumps(step)}")

    def run_step(self, full_step, ctx, track):
        """Run one step: operate, track what it created, save values, check expectations."""
        # A template's {i} is filled in per record, below.
        raw_step = {k: v for k, v in full_step.items() if k != "template"}
        as_names = raw_step.get("as") or []
        later = set(raw_step.get("save") or {}) | set(as_names if isinstance(as_names, list) else [as_names])
        step = substitute(raw_step, {**ctx, "later": later})
        if "template" in full_step:
            step["records"] = [
                substitute(full_step["template"], {**ctx, "saved": {**ctx["saved"], "i": f"{i:04d}"}})
                for i in range(step["count"])
            ]
        try:
            out = self.operate(step)
        except SalesforceError as err:
            if not step.get("error"):
                raise
            match_error(step["error"], normalise_error(err))
            return
        track(out.get("created") or [])
        if step.get("error") and "request" not in step:
            raise AssertionError(f"expected {json.dumps(step['error'])}, but the call succeeded")

        if step.get("as"):
            names = step["as"] if isinstance(step["as"], list) else [step["as"]]
            ids = [r.get("id") for r in out["results"]] if "results" in out else [out["value"].get("id")]
            for name, rid in zip(names, ids):
                ctx["saved"][name] = rid
        if "query" in step:
            subject = out["value"]["records"][0] if out["value"]["records"] else MISSING
        else:
            subject = out["value"]
        for name, path in (step.get("save") or {}).items():
            value = dig(subject, path)
            ctx["saved"][name] = None if value is MISSING else value
        # Expectations can refer to values saved by this step.
        expect = substitute(raw_step, ctx)

        if "request" in step:
            if "status" in expect:
                match(expect["status"], out["status"], "status")
            if expect.get("error"):
                body = out["value"]
                first = body[0] if isinstance(body, list) and body and isinstance(body[0], dict) else {}
                match_error(
                    expect["error"],
                    {"code": first.get("errorCode"), "fields": first.get("fields") or [], "message": first.get("message")},
                )
            if "response" in expect:
                match(expect["response"], out["value"], "body")
            for name, pattern in (expect.get("response_headers") or {}).items():
                value = out["headers"].get(name)
                if pattern == "$absent":
                    match("$absent", MISSING if value is None else value, f"header {name}")
                else:
                    match({"$regex": pattern}, value, f"header {name}")
            return
        if "record" in expect and "retrieve" in step:
            match(expect["record"], out["value"], "record")
        if "created" in expect:
            match(expect["created"], out["value"].get("created", MISSING), "created")
        if "results" in expect:
            match_results(expect["results"], out["results"])
        if "query" in step:
            recs = out["value"]["records"]
            if "total" in expect:
                match(expect["total"], out["value"]["totalSize"], "totalSize")
            if "records" in expect:
                exp = {"$unordered": expect["records"]} if expect.get("unordered") else expect["records"]
                match(exp, recs, "records", True)
            if "pluck" in expect:
                values = []
                for r in recs:
                    v = dig(r, expect["pluck"])
                    values.append(None if v is MISSING else v)
                where = f"values of {expect['pluck']}"
                if "values" in expect:
                    exp = {"$unordered": expect["values"]} if expect.get("unordered") else expect["values"]
                    match(exp, values, where)
                if "contains" in expect:
                    match({"$contains": expect["contains"]}, values, where)
                for v in expect.get("excludes") or []:
                    if v in values:
                        match("$absent", v, f"{where} include {v}")
        if "retrieve_many" in step and "records" in expect:
            match(expect["records"], out["value"], "records")
        if "describe" in step or "describe_global" in step:
            d = out["value"]
            if expect.get("object"):
                match(expect["object"], d, "describe")
            for name, exp in (expect.get("fields") or {}).items():
                found = next((f for f in d["fields"] if f["name"] == name), MISSING)
                match(exp, found, f"field {name}")
            for name, exp in (expect.get("child_relationships") or {}).items():
                found = next((c for c in d["childRelationships"] if c.get("relationshipName") == name), MISSING)
                match(exp, found, f"child relationship {name}")
            for name, exp in (expect.get("sobjects") or {}).items():
                found = next((s for s in d["sobjects"] if s["name"] == name), MISSING)
                match(exp, found, f"sobject {name}")
        if "composite" in step and expect.get("responses"):
            match(expect["responses"], out["value"], "compositeResponse")

    def clean_up(self, ids):
        """Delete records newest first, ignoring failures (some are already gone)."""
        ids = [i for i in reversed(ids) if i]
        for i in range(0, len(ids), 200):
            try:
                self.raw("DELETE", f"/composite/sobjects?allOrNone=false&ids={','.join(ids[i : i + 200])}")
            except requests.RequestException:
                pass

    def run_steps(self, steps, ctx, ids):
        for i, step in enumerate(steps):
            try:
                self.run_step(step, ctx, ids.extend)
            except Exception as err:
                op = next(iter(step))
                raise type(err)(f"step {i + 1} ({op}): {err}") if isinstance(err, AssertionError) else err


@pytest.fixture(scope="session")
def runner(sf, target):
    return Runner(sf, target)


@pytest.fixture(scope="session")
def file_contexts(runner, users):
    """Each spec file's context, created (running its setup) on first use;
    the setup's records are deleted at the end of the session."""
    contexts = {}
    setup_ids = []

    def get(spec):
        name = spec["file"]
        if name not in contexts:
            ctx = {"tag": new_tag(), "user": users["user"], "user2": users["user2"], "version": API_VERSION, "saved": {}}
            try:
                runner.run_steps(spec["setup"], ctx, setup_ids)
                contexts[name] = (ctx, None)
            except Exception as err:  # every case in the file reports it
                contexts[name] = (ctx, err)
        ctx, err = contexts[name]
        if err is not None:
            raise AssertionError(f"setup of {name} failed: {err}") from err
        return ctx

    yield get
    runner.clean_up(setup_ids)


@pytest.mark.parametrize("spec,case", CASES)
def test_conformance(spec, case, runner, file_contexts, users, target):
    if "python" in (case.get("skip") or {}):
        pytest.skip(case["skip"]["python"])
    if "user2" in (case.get("needs") or []) and not users["user2"]:
        pytest.skip("needs a second user (LF_SECOND_USERNAME)")
    todo = case.get("todo") if target.localforce else None
    file_ctx = file_contexts(spec)
    ctx = {**file_ctx, "saved": dict(file_ctx["saved"])}
    ids = []
    try:
        runner.run_steps(case["steps"], ctx, ids)
    except Exception as err:
        if todo:
            pytest.xfail(f"not in localforce yet: {todo}: {err}")
        raise
    finally:
        runner.clean_up(ids)
