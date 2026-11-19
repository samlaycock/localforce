"""The Bulk APIs through simple-salesforce: `sf.bulk` (Bulk API 1.0 with JSON
jobs) and `sf.bulk2` (Bulk API 2.0: CSV ingest jobs and query jobs)."""

import contextlib
import csv
import io
import itertools

import pytest
from simple_salesforce.exceptions import SalesforceMalformedRequest, SalesforceOperationError

_keys = itertools.count(1)


@pytest.fixture
def track(sf):
    """`track(*ids)` deletes the records after the test, newest first."""
    ids = []
    yield ids.extend
    ids.reverse()
    for i in range(0, len(ids), 200):
        with contextlib.suppress(Exception):
            sf.restful("composite/sobjects", method="DELETE", params={"ids": ",".join(ids[i : i + 200]), "allOrNone": "false"})


@pytest.fixture
def wait(target):
    """Seconds simple-salesforce waits between polls: localforce finishes jobs at once."""
    return 0.05 if target.localforce else 2


@pytest.fixture
def hard_delete(target):
    if not target.localforce:
        pytest.skip("needs the Bulk API Hard Delete permission, which orgs don't grant by default")


def key(tag):
    return f"{tag}-{next(_keys)}"


def rows(text, delimiter=","):
    return list(csv.DictReader(io.StringIO(text), delimiter=delimiter))


def names(sf, soql):
    return sorted(r["Name"] for r in sf.query_all(soql)["records"])


def create(sf, track, records):
    res = sf.restful("composite/sobjects", method="POST", json={"allOrNone": True, "records": [{"attributes": {"type": "Account"}, **r} for r in records]})
    assert all(r["success"] for r in res), res
    ids = [r["id"] for r in res]
    track(ids)
    return ids


# --- Bulk API 1.0 (sf.bulk) -------------------------------------------------


def test_bulk_insert_returns_a_result_per_record(sf, tag, track):
    res = sf.bulk.Account.insert([{"Name": f"{tag} b1 one", "Tier__c": "Gold"}, {"Name": f"{tag} b1 two", "NumberOfEmployees": 5}, {"Tier__c": "Gold"}])
    track([r["id"] for r in res if r["id"]])
    assert [r["success"] for r in res] == [True, True, False]
    assert [r["created"] for r in res[:2]] == [True, True]
    assert res[0]["id"].startswith("001") and len(res[0]["id"]) == 18
    assert res[2]["errors"][0]["statusCode"] == "REQUIRED_FIELD_MISSING"
    assert res[2]["errors"][0]["fields"] == ["Name"]
    found = sf.query(f"SELECT Name, Tier__c, NumberOfEmployees FROM Account WHERE Name LIKE '{tag} b1 %' ORDER BY Name")["records"]
    assert [(r["Name"], r["Tier__c"], r["NumberOfEmployees"]) for r in found] == [(f"{tag} b1 one", "Gold", None), (f"{tag} b1 two", None, 5)]


def test_bulk_insert_in_several_batches(sf, tag, track):
    res = sf.bulk.Account.insert([{"Name": f"{tag} b1 batch {i:03}"} for i in range(25)], batch_size=10)
    track([r["id"] for r in res])
    assert len(res) == 25 and all(r["success"] for r in res)
    assert len(names(sf, f"SELECT Name FROM Account WHERE Name LIKE '{tag} b1 batch %'")) == 25


def test_bulk_update_sets_and_clears_fields(sf, tag, track):
    [a] = create(sf, track, [{"Name": f"{tag} b1 update", "Tier__c": "Gold"}])
    res = sf.bulk.Account.update([{"Id": a, "Tier__c": None, "NumberOfEmployees": 9}])
    assert res == [{"success": True, "created": False, "id": a, "errors": []}]
    rec = sf.Account.get(a)
    assert rec["Tier__c"] is None
    assert rec["NumberOfEmployees"] == 9


def test_bulk_upsert_by_external_id(sf, tag, track):
    old, new = key(tag), key(tag)
    [a] = create(sf, track, [{"Name": f"{tag} b1 upsert old", "External_Key__c": old}])
    res = sf.bulk.Account.upsert(
        [{"External_Key__c": old, "Name": f"{tag} b1 upserted"}, {"External_Key__c": new, "Name": f"{tag} b1 upsert new"}],
        "External_Key__c",
    )
    track([r["id"] for r in res])
    assert [(r["success"], r["created"]) for r in res] == [(True, False), (True, True)]
    assert res[0]["id"] == a
    assert sf.Account.get_by_custom_id("External_Key__c", new)["Name"] == f"{tag} b1 upsert new"


def test_bulk_upsert_with_a_parent_external_id(sf, tag, track):
    k = key(tag)
    [account] = create(sf, track, [{"Name": f"{tag} b1 parent", "External_Key__c": k}])
    res = sf.bulk.Contact.insert([{"LastName": f"{tag} b1 child", "Account": {"External_Key__c": k}}])
    track([r["id"] for r in res if r["id"]])
    assert res[0]["success"], res
    assert sf.Contact.get(res[0]["id"])["AccountId"] == account


def test_bulk_delete_and_include_detailed_results(sf, tag, track):
    ids = create(sf, track, [{"Name": f"{tag} b1 delete {i}"} for i in range(2)])
    res = sf.bulk.Account.delete([{"Id": i} for i in ids], include_detailed_results=True)
    assert sorted(r["id"] for r in res) == sorted(ids)
    assert all(r["success"] for r in res)
    # The detailed results add the request's fields to each result.
    assert sorted(r["Id"] for r in res) == sorted(ids)
    assert sf.query(f"SELECT Id FROM Account WHERE Name LIKE '{tag} b1 delete %'")["totalSize"] == 0
    assert sf.query_all(f"SELECT Id FROM Account WHERE Name LIKE '{tag} b1 delete %'", include_deleted=True)["totalSize"] == 2


def test_bulk_hard_delete(sf, tag, track, hard_delete):
    ids = create(sf, track, [{"Name": f"{tag} b1 hard"}])
    res = sf.bulk.Account.hard_delete([{"Id": ids[0]}])
    assert res[0]["success"], res
    assert sf.query_all(f"SELECT Id FROM Account WHERE Id = '{ids[0]}'", include_deleted=True)["totalSize"] == 0


def test_bulk_query(sf, tag, track):
    k = key(tag)
    [account] = create(sf, track, [{"Name": f"{tag} b1q parent", "External_Key__c": k}])
    res = sf.restful("composite/sobjects", method="POST", json={"allOrNone": True, "records": [
        {"attributes": {"type": "Contact"}, "LastName": f"{tag} b1q child", "AccountId": account},
        {"attributes": {"type": "Contact"}, "LastName": f"{tag} b1q orphan"},
    ]})
    track([r["id"] for r in res])
    records = sf.bulk.Contact.query(f"SELECT Id, LastName, Account.Name, CreatedDate FROM Contact WHERE LastName LIKE '{tag} b1q %' ORDER BY LastName")
    assert [r["LastName"] for r in records] == [f"{tag} b1q child", f"{tag} b1q orphan"]
    assert records[0]["attributes"]["type"] == "Contact"
    assert records[0]["Account"]["Name"] == f"{tag} b1q parent"
    assert records[1]["Account"] is None
    # Bulk API 1.0's JSON gives datetimes as epoch milliseconds.
    assert isinstance(records[0]["CreatedDate"], int)


def test_bulk_query_all_includes_deleted_records(sf, tag, track):
    [a] = create(sf, track, [{"Name": f"{tag} b1q deleted"}])
    sf.Account.delete(a)
    soql = f"SELECT Id, IsDeleted FROM Account WHERE Id = '{a}'"
    assert sf.bulk.Account.query(soql) == []
    [rec] = sf.bulk.Account.query_all(soql)
    assert rec["IsDeleted"] is True


def test_bulk_errors(sf, target):
    with pytest.raises(SalesforceMalformedRequest) as err:
        sf.bulk.No_Such_Object__c.insert([{"Name": "x"}])
    assert err.value.content["exceptionCode"] == "InvalidJob"
    res = target.session.post(
        f"{target.instance_url}/services/async/{sf.sf_version}/job",
        headers={"X-SFDC-Session": "not-a-session", "Content-Type": "application/json"},
        json={"operation": "insert", "object": "Account", "contentType": "JSON"},
    )
    assert res.status_code in (400, 401)
    assert res.json()["exceptionCode"] == "InvalidSessionId"


# --- Bulk API 2.0 (sf.bulk2) ------------------------------------------------


def test_bulk2_insert_and_results(sf, tag, track, wait):
    [res] = sf.bulk2.Account.insert(records=[{"Name": f"{tag} b2 one", "Tier__c": "Gold"}, {"Name": f"{tag} b2 two, with a comma"}, {"Tier__c": "Gold"}], wait=wait)
    assert res["numberRecordsProcessed"] == 3
    assert res["numberRecordsFailed"] == 1
    assert res["numberRecordsTotal"] == 3
    ok = rows(sf.bulk2.Account.get_successful_records(res["job_id"]))
    track([r["sf__Id"] for r in ok])
    assert sorted(r["Name"] for r in ok) == [f"{tag} b2 one", f"{tag} b2 two, with a comma"]
    assert {r["sf__Created"] for r in ok} == {"true"}
    [failed] = rows(sf.bulk2.Account.get_failed_records(res["job_id"]))
    assert failed["sf__Error"].startswith("REQUIRED_FIELD_MISSING:Required fields are missing: [Name]")
    assert failed["Tier__c"] == "Gold"
    assert rows(sf.bulk2.Account.get_unprocessed_records(res["job_id"])) == []
    assert names(sf, f"SELECT Name FROM Account WHERE Name LIKE '{tag} b2 %'") == [f"{tag} b2 one", f"{tag} b2 two, with a comma"]


def test_bulk2_insert_from_a_csv_file_in_batches(sf, tag, track, wait, tmp_path):
    path = tmp_path / "accounts.csv"
    path.write_text("Name,NumberOfEmployees\n" + "".join(f"{tag} b2 file {i:03},{i}\n" for i in range(30)))
    res = sf.bulk2.Account.insert(csv_file=str(path), batch_size=10, wait=wait)
    assert [r["numberRecordsProcessed"] for r in res] == [10, 10, 10]
    for r in res:
        track([x["sf__Id"] for x in rows(sf.bulk2.Account.get_successful_records(r["job_id"]))])
    assert len(names(sf, f"SELECT Name FROM Account WHERE Name LIKE '{tag} b2 file %'")) == 30


def test_bulk2_update_upsert_and_delete(sf, tag, track, wait):
    k1, k2 = key(tag), key(tag)
    [a] = create(sf, track, [{"Name": f"{tag} b2 old", "External_Key__c": k1, "Tier__c": "Gold"}])

    # An empty value leaves a field alone.
    [res] = sf.bulk2.Account.update(records=[{"Id": a, "NumberOfEmployees": "4", "Tier__c": ""}], wait=wait)
    assert (res["numberRecordsProcessed"], res["numberRecordsFailed"]) == (1, 0)
    rec = sf.Account.get(a)
    assert (rec["NumberOfEmployees"], rec["Tier__c"]) == (4, "Gold")

    [res] = sf.bulk2.Account.upsert(
        records=[{"External_Key__c": k1, "Name": f"{tag} b2 upserted"}, {"External_Key__c": k2, "Name": f"{tag} b2 upsert new"}],
        external_id_field="External_Key__c",
        wait=wait,
    )
    assert (res["numberRecordsProcessed"], res["numberRecordsFailed"]) == (2, 0)
    ok = {r["External_Key__c"]: r for r in rows(sf.bulk2.Account.get_successful_records(res["job_id"]))}
    track([r["sf__Id"] for r in ok.values()])
    assert (ok[k1]["sf__Id"], ok[k1]["sf__Created"]) == (a, "false")
    assert ok[k2]["sf__Created"] == "true"

    [res] = sf.bulk2.Account.delete(records=[{"Id": r["sf__Id"]} for r in ok.values()], wait=wait)
    assert (res["numberRecordsProcessed"], res["numberRecordsFailed"]) == (2, 0)
    assert sf.query(f"SELECT Id FROM Account WHERE External_Key__c IN ('{k1}', '{k2}')")["totalSize"] == 0


def test_bulk2_hard_delete(sf, tag, track, wait, hard_delete):
    ids = create(sf, track, [{"Name": f"{tag} b2 hard"}])
    [res] = sf.bulk2.Account.hard_delete(records=[{"Id": ids[0]}], wait=wait)
    assert (res["numberRecordsProcessed"], res["numberRecordsFailed"]) == (1, 0)
    assert sf.query_all(f"SELECT Id FROM Account WHERE Id = '{ids[0]}'", include_deleted=True)["totalSize"] == 0


def test_bulk2_tab_delimited(sf, tag, track, wait, tmp_path):
    path = tmp_path / "accounts.tsv"
    path.write_text(f"Name\tBillingCity\n{tag} b2 tab\tSan Jose, CA\n")
    [res] = sf.bulk2.Account.insert(csv_file=str(path), column_delimiter="TAB", wait=wait)
    assert res["numberRecordsFailed"] == 0
    [ok] = rows(sf.bulk2.Account.get_successful_records(res["job_id"]), delimiter="\t")
    track([ok["sf__Id"]])
    assert sf.Account.get(ok["sf__Id"])["BillingCity"] == "San Jose, CA"


def test_bulk2_query_pages(sf, tag, track, wait):
    n = 1205
    [res] = sf.bulk2.Account.insert(records=[{"Name": f"{tag} b2q {i:05}"} for i in range(n)], wait=wait)
    track([r["sf__Id"] for r in rows(sf.bulk2.Account.get_successful_records(res["job_id"]))])
    assert res["numberRecordsFailed"] == 0
    pages = list(sf.bulk2.Account.query(f"SELECT Id, Name, Owner.Name FROM Account WHERE Name LIKE '{tag} b2q %'", max_records=500, wait=wait))
    assert len(pages) == 3
    found = [r for page in pages for r in rows(page)]
    assert len(found) == n
    assert len({r["Id"] for r in found}) == n
    assert list(found[0].keys()) == ["Id", "Name", "Owner.Name"]
    assert found[0]["Owner.Name"]


def test_bulk2_query_all(sf, tag, track, wait):
    [a] = create(sf, track, [{"Name": f"{tag} b2q deleted"}])
    sf.Account.delete(a)
    soql = f"SELECT Id, Name FROM Account WHERE Id = '{a}'"
    assert [r for page in sf.bulk2.Account.query(soql, wait=wait) for r in rows(page)] == []
    [rec] = [r for page in sf.bulk2.Account.query_all(soql, wait=wait) for r in rows(page)]
    assert rec["Name"] == f"{tag} b2q deleted"


def test_bulk2_download(sf, tag, track, wait, tmp_path):
    create(sf, track, [{"Name": f"{tag} b2 download {i}"} for i in range(3)])
    res = sf.bulk2.Account.download(f"SELECT Name FROM Account WHERE Name LIKE '{tag} b2 download %'", path=str(tmp_path), max_records=2, wait=wait)
    assert [r["number_of_records"] for r in res] == [2, 1]
    found = [row["Name"] for r in res for row in csv.DictReader(open(r["file"]))]
    assert sorted(found) == [f"{tag} b2 download {i}" for i in range(3)]


def test_bulk2_errors(sf, wait):
    with pytest.raises(SalesforceMalformedRequest):
        sf.bulk2.No_Such_Object__c.insert(records=[{"Name": "x"}], wait=wait)
    with pytest.raises(SalesforceMalformedRequest) as err:
        list(sf.bulk2.Account.query("SELECT No_Such_Field__c FROM Account", wait=wait))
    assert err.value.content[0]["errorCode"] == "INVALID_FIELD"


def test_bulk2_a_job_with_an_unknown_column_fails(sf, tag, wait):
    with pytest.raises(SalesforceOperationError, match="No_Such_Field__c"):
        sf.bulk2.Account.insert(records=[{"Name": f"{tag} b2 bad column", "No_Such_Field__c": "x"}], wait=wait)
    assert sf.query(f"SELECT Id FROM Account WHERE Name = '{tag} b2 bad column'")["totalSize"] == 0
