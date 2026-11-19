"""simple-salesforce APIs the conformance spec can't express: logging in,
paging helpers, its return values, error classes, API usage, number parsing,
search, replication and the Tooling API. The Bulk APIs are in test_bulk.py.
"""

import contextlib
import time
from datetime import datetime, timedelta, timezone
from decimal import Decimal

import pytest
from simple_salesforce import Salesforce
from simple_salesforce.exceptions import (
    SalesforceAuthenticationFailed,
    SalesforceExpiredSession,
    SalesforceMalformedRequest,
    SalesforceMoreThanOneRecord,
    SalesforceResourceNotFound,
)

from conftest import API_VERSION


@pytest.fixture
def track(sf):
    """`track(*ids)` deletes the records after the test, newest first."""
    ids = []
    yield ids.extend
    ids.reverse()
    for i in range(0, len(ids), 200):
        with contextlib.suppress(Exception):
            sf.restful("composite/sobjects", method="DELETE", params={"ids": ",".join(ids[i : i + 200]), "allOrNone": "false"})


def create_many(sf, records):
    """Create records (each with attributes.type) through sObject Collections; returns their ids."""
    ids = []
    for i in range(0, len(records), 200):
        res = sf.restful("composite/sobjects", method="POST", json={"allOrNone": True, "records": records[i : i + 200]})
        assert all(r["success"] for r in res), res
        ids.extend(r["id"] for r in res)
    return ids


def needs_login(target, *fields):
    missing = [f for f in fields if not getattr(target, f)]
    if missing:
        pytest.skip(f"needs {', '.join(missing)} to log in (SF_USERNAME, SF_PASSWORD, SF_CLIENT_ID, SF_CLIENT_SECRET)")


def assert_works(sf, users):
    res = sf.query(f"SELECT Id FROM User WHERE Id = '{users['user']}'")
    assert res["totalSize"] == 1


# Logging in


def test_direct_session_and_instance_url(target, users):
    sf = Salesforce(instance_url=target.instance_url, session_id=target.access_token, session=target.session, version=API_VERSION)
    assert sf.session_id == target.access_token
    assert sf.base_url.endswith(f"/services/data/v{API_VERSION}/")
    assert_works(sf, users)


def test_soap_login_with_security_token(target, login_session, login_domain, users):
    needs_login(target, "username", "password")
    soap_login(target, login_session, login_domain, users)


def soap_login(target, login_session, login_domain, users):
    sf = Salesforce(
        username=target.username,
        password=target.password,
        security_token="",
        domain=login_domain,
        session=login_session,
        version=API_VERSION,
    )
    assert sf.session_id
    assert sf.auth_type == "password"
    assert_works(sf, users)


def test_oauth_password_flow_with_consumer_key(target, login_session, login_domain, users):
    needs_login(target, "username", "password", "client_id", "client_secret")
    sf = Salesforce(
        username=target.username,
        password=target.password,
        consumer_key=target.client_id,
        consumer_secret=target.client_secret,
        domain=login_domain,
        session=login_session,
        version=API_VERSION,
    )
    assert sf.session_id
    assert_works(sf, users)


def test_client_credentials_flow(target, login_session):
    if not target.localforce:
        pytest.skip("needs a connected app with a run-as user")
    # simple-salesforce only uses client credentials with a My Domain (not
    # login or test), and sends the key and secret with HTTP Basic auth.
    sf = Salesforce(
        consumer_key=target.client_id,
        consumer_secret=target.client_secret,
        domain="localforce.my",
        session=login_session,
        version=API_VERSION,
    )
    assert sf.auth_type == "client-credentials"
    assert sf.query("SELECT Id FROM User LIMIT 1")["totalSize"] == 1


def rsa_key_pem() -> str:
    """A throwaway RSA private key for signing JWT bearer assertions."""
    from cryptography.hazmat.primitives import serialization
    from cryptography.hazmat.primitives.asymmetric import rsa

    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    return key.private_bytes(
        serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()
    ).decode()


def test_jwt_bearer_flow(target, login_session, login_domain, users):
    if not target.localforce:
        pytest.skip("needs a connected app with a certificate")
    needs_login(target, "username")
    sf = Salesforce(
        username=target.username,
        consumer_key="localforce-tests",
        privatekey=rsa_key_pem(),
        domain=login_domain,
        session=login_session,
        version=API_VERSION,
    )
    assert sf.session_id
    assert_works(sf, users)


# simple-salesforce warns about self-authorization on "user hasn't approved this consumer".
@pytest.mark.filterwarnings("ignore::UserWarning")
def test_jwt_bearer_flow_with_an_unknown_user_fails(target, login_session, login_domain):
    if not target.localforce:
        pytest.skip("needs a connected app with a certificate")
    with pytest.raises(SalesforceAuthenticationFailed) as err:
        Salesforce(
            username="nobody-at-all@example.invalid",
            consumer_key="localforce-tests",
            privatekey=rsa_key_pem(),
            domain=login_domain,
            session=login_session,
        )
    assert err.value.code == "invalid_grant"


def test_soap_login_with_an_unknown_user_fails(target, login_session, login_domain):
    needs_login(target, "username")
    with pytest.raises(SalesforceAuthenticationFailed) as err:
        Salesforce(
            username="nobody-at-all@example.invalid",
            password="x",
            security_token="",
            domain=login_domain,
            session=login_session,
            version=API_VERSION,
        )
    # simple-salesforce reads the code from the fault's <sf:exceptionCode>.
    assert err.value.code == "INVALID_LOGIN"


def test_oauth_login_with_an_unknown_user_fails(target, login_session, login_domain):
    needs_login(target, "client_id", "client_secret")
    with pytest.raises(SalesforceAuthenticationFailed) as err:
        Salesforce(
            username="nobody-at-all@example.invalid",
            password="x",
            consumer_key=target.client_id,
            consumer_secret=target.client_secret,
            domain=login_domain,
            session=login_session,
        )
    assert err.value.code == "invalid_grant"


def test_an_expired_session_is_renewed_by_logging_in_again(target, login_session, login_domain, users):
    needs_login(target, "username", "password", "client_id", "client_secret")
    sf = Salesforce(
        username=target.username,
        password=target.password,
        consumer_key=target.client_id,
        consumer_secret=target.client_secret,
        domain=login_domain,
        session=login_session,
        version=API_VERSION,
    )
    old = sf.session_id
    revoked = login_session.post(f"https://{sf.sf_instance}/services/oauth2/revoke", data={"token": old})
    assert revoked.status_code == 200
    # simple-salesforce sees INVALID_SESSION_ID, logs in again and retries.
    assert_works(sf, users)
    assert sf.session_id != old


# Queries


@pytest.fixture(scope="module")
def contacts(sf, tag):
    """250 Contacts named "<tag> page NNNN"."""
    ids = create_many(sf, [{"attributes": {"type": "Contact"}, "LastName": f"{tag} page {i:04d}"} for i in range(250)])
    yield ids
    for i in range(0, len(ids), 200):
        with contextlib.suppress(Exception):
            sf.restful("composite/sobjects", method="DELETE", params={"ids": ",".join(ids[i : i + 200]), "allOrNone": "false"})


BATCH_200 = {"Sforce-Query-Options": "batchSize=200"}


def page_soql(tag):
    return f"SELECT Id, LastName FROM Contact WHERE LastName LIKE '{tag} page %' ORDER BY LastName"


def test_query_pages_with_batch_size_and_query_more(sf, tag, contacts):
    first = sf.query(page_soql(tag), headers=BATCH_200)
    assert first["totalSize"] == 250
    assert first["done"] is False
    assert len(first["records"]) == 200
    assert first["nextRecordsUrl"].startswith(f"/services/data/v{API_VERSION}/query/")

    # By URL ...
    by_url = sf.query_more(first["nextRecordsUrl"], identifier_is_url=True)
    assert by_url["done"] is True
    assert len(by_url["records"]) == 50
    # ... and, for a new query, by the locator alone.
    again = sf.query(page_soql(tag), headers=BATCH_200)
    by_id = sf.query_more(again["nextRecordsUrl"].rsplit("/", 1)[1])
    assert [r["Id"] for r in by_id["records"]] == [r["Id"] for r in by_url["records"]]

    names = [r["LastName"] for r in first["records"] + by_url["records"]]
    assert names == sorted(names)
    assert {r["Id"] for r in first["records"] + by_url["records"]} == set(contacts)


def test_query_all_iter_follows_every_page(sf, tag, contacts):
    records = list(sf.query_all_iter(page_soql(tag), headers=BATCH_200))
    assert len(records) == 250
    assert {r["Id"] for r in records} == set(contacts)


def test_query_all_counts_the_records_it_fetched(sf, tag, contacts):
    res = sf.query_all(page_soql(tag), headers=BATCH_200)
    assert res["totalSize"] == 250
    assert res["done"] is True
    assert len(res["records"]) == 250


def test_include_deleted_uses_query_all(sf, tag, track):
    ids = create_many(sf, [{"attributes": {"type": "Account"}, "Name": f"{tag} deleted {i}"} for i in range(2)])
    track(ids)
    sf.Account.delete(ids[0])
    soql = f"SELECT Id, IsDeleted FROM Account WHERE Name LIKE '{tag} deleted %'"
    assert [r["Id"] for r in sf.query(soql)["records"]] == [ids[1]]
    everything = sf.query_all(soql, include_deleted=True)["records"]
    assert {r["Id"]: r["IsDeleted"] for r in everything} == {ids[0]: True, ids[1]: False}


def test_query_errors_are_malformed_request(sf):
    with pytest.raises(SalesforceMalformedRequest) as err:
        sf.query("SELECT Id FROM Account WHERE")
    assert err.value.status == 400
    assert err.value.content[0]["errorCode"] == "MALFORMED_QUERY"


# Records


def test_create_get_update_delete_return_values(sf, tag, track):
    created = sf.Account.create({"Name": f"{tag} crud"})
    track([created["id"]])
    assert created["success"] is True
    assert created["errors"] == []
    record = sf.Account.get(created["id"])
    assert record["Name"] == f"{tag} crud"
    assert record["attributes"]["type"] == "Account"
    assert sf.Account.update(created["id"], {"Phone": "555-0100"}) == 204
    assert sf.Account.get(created["id"])["Phone"] == "555-0100"
    response = sf.Account.update(created["id"], {"Phone": "555-0101"}, raw_response=True)
    assert response.status_code == 204
    assert sf.Account.delete(created["id"]) == 204
    with pytest.raises(SalesforceResourceNotFound) as err:
        sf.Account.get(created["id"])
    assert err.value.content[0]["errorCode"] in ("ENTITY_IS_DELETED", "NOT_FOUND")


def test_get_by_custom_id(sf, tag, track):
    key = f"{tag}-GET"
    created = sf.Account.create({"Name": f"{tag} by key", "External_Key__c": key})
    track([created["id"]])
    assert sf.Account.get_by_custom_id("External_Key__c", key)["Id"] == created["id"]


def test_get_by_custom_id_with_two_matches_is_more_than_one_record(sf, tag, track):
    code = f"{tag}-DUP"
    track(create_many(sf, [{"attributes": {"type": "Project__c"}, "Name": f"{tag} dup {i}", "Code__c": code} for i in range(2)]))
    with pytest.raises(SalesforceMoreThanOneRecord) as err:
        sf.Project__c.get_by_custom_id("Code__c", code)
    assert err.value.status == 300
    assert len(err.value.content) == 2


def test_upsert_returns_201_when_it_creates_and_2xx_when_it_updates(sf, tag, track):
    key = f"{tag}-UPS"
    created = sf.Account.upsert(f"External_Key__c/{key}", {"Name": f"{tag} upsert"}, raw_response=True)
    assert created.status_code == 201
    body = created.json()
    track([body["id"]])
    assert body["created"] is True
    # Updates are 204 No Content, or (API 46.0 and later) 200 with created: false.
    updated = sf.Account.upsert(f"External_Key__c/{key}", {"Phone": "1"}, raw_response=True)
    assert updated.status_code in (200, 204)
    if updated.status_code == 200:
        assert updated.json()["created"] is False
        assert updated.json()["id"] == body["id"]
    assert sf.Account.upsert(f"External_Key__c/{key}", {"Phone": "2"}) in (200, 204)
    assert sf.Account.get(body["id"])["Phone"] == "2"


def test_errors_carry_the_salesforce_error_body(sf, tag):
    with pytest.raises(SalesforceMalformedRequest) as err:
        sf.Account.create({"Phone": f"{tag} no name"})
    assert err.value.status == 400
    assert err.value.content[0]["errorCode"] == "REQUIRED_FIELD_MISSING"
    assert err.value.content[0]["fields"] == ["Name"]
    with pytest.raises(SalesforceResourceNotFound) as err:
        sf.Account.get("001000000000000AAA")
    assert err.value.status == 404
    assert err.value.content[0]["errorCode"] == "NOT_FOUND"


def test_an_invalid_session_is_expired_session(target):
    sf = Salesforce(
        instance_url=target.instance_url,
        session_id="00Dxx0000000000!not-a-real-session",
        session=target.session,
        version=API_VERSION,
    )
    with pytest.raises(SalesforceExpiredSession) as err:
        sf.query("SELECT Id FROM User LIMIT 1")
    assert err.value.status == 401
    assert err.value.content[0]["errorCode"] == "INVALID_SESSION_ID"


def test_parse_float_decimal_keeps_currency_exact(target, tag, track):
    sf = target.client(parse_float=Decimal)
    created = sf.Project__c.create({"Name": f"{tag} decimal", "Budget__c": 1234.56})
    track([created["id"]])
    [row] = sf.query(f"SELECT Budget__c FROM Project__c WHERE Id = '{created['id']}'")["records"]
    assert row["Budget__c"] == Decimal("1234.56")
    # sf.<Object> doesn't pass parse_float on (simple-salesforce 1.12), so
    # SFType methods still return floats.
    assert sf.Project__c.get(created["id"])["Budget__c"] == 1234.56


# Metadata, limits and generic calls


def test_metadata_and_describe(sf):
    meta = sf.Project__c.metadata()
    assert meta["objectDescribe"]["name"] == "Project__c"
    assert "recentItems" in meta
    describe = sf.Project__c.describe()
    assert any(f["name"] == "Budget__c" and f["type"] == "currency" for f in describe["fields"])
    assert any(s["name"] == "Project__c" for s in sf.describe()["sobjects"])


def test_limits_and_api_usage(sf):
    limits = sf.limits()
    assert limits["DailyApiRequests"]["Max"] > 0
    sf.query("SELECT Id FROM User LIMIT 1")
    usage = sf.api_usage["api-usage"]
    assert 0 < usage.used <= usage.total
    before = usage.used
    sf.query("SELECT Id FROM User LIMIT 1")
    assert sf.api_usage["api-usage"].used > before


def test_restful(sf, tag, track):
    versions_root = sf.restful("")
    assert versions_root["sobjects"] == f"/services/data/v{API_VERSION}/sobjects"
    created = sf.restful("sobjects/Account", method="POST", json={"Name": f"{tag} restful"})
    track([created["id"]])
    # 204 No Content comes back as None.
    assert sf.restful(f"sobjects/Account/{created['id']}", method="PATCH", json={"Phone": "1"}) is None
    assert sf.restful(f"sobjects/Account/{created['id']}", params={"fields": "Phone"})["Phone"] == "1"


def test_oauth2_userinfo(sf, users):
    assert sf.oauth2("userinfo")["user_id"] == users["user"]


# Search, replication and the Tooling API


def test_sosl_search(sf, tag, track, target):
    created = sf.Account.create({"Name": f"{tag} searchable"})
    track([created["id"]])
    if not target.localforce:
        time.sleep(20)  # a real org indexes new records for search after a delay
    res = sf.search(f"FIND {{{tag}}} IN NAME FIELDS RETURNING Account(Id, Name)")
    assert [r["Id"] for r in res["searchRecords"]] == [created["id"]]
    assert res["searchRecords"][0]["Name"] == f"{tag} searchable"
    quick = sf.quick_search(tag)
    assert any(r["Id"] == created["id"] for r in quick["searchRecords"])


def test_sosl_search_errors(sf):
    with pytest.raises(SalesforceMalformedRequest) as err:
        sf.search("FIND nothing-in-braces")
    assert err.value.content[0]["errorCode"] == "MALFORMED_SEARCH"


def test_updated_and_deleted(sf, tag, track):
    start = datetime.now(timezone.utc) - timedelta(minutes=1)
    created = sf.Account.create({"Name": f"{tag} replicated"})
    track([created["id"]])
    end = datetime.now(timezone.utc) + timedelta(minutes=1)
    updated = sf.Account.updated(start, end)
    assert created["id"] in updated["ids"]
    assert "latestDateCovered" in updated
    sf.Account.delete(created["id"])
    deleted = sf.Account.deleted(start, end)["deletedRecords"]
    assert any(r["id"] == created["id"] and r["deletedDate"] for r in deleted)


def test_apex_rest_without_a_matching_class_is_not_found(sf):
    # There's no Apex in the fixture schema, so this checks the error simple-salesforce surfaces.
    with pytest.raises(SalesforceResourceNotFound) as err:
        sf.apexecute(f"no/such/endpoint/{API_VERSION}", method="GET")
    assert err.value.content[0]["errorCode"] == "NOT_FOUND"


def test_tooling_api(sf):
    res = sf.toolingexecute("query/?q=SELECT+Id+FROM+ApexClass+LIMIT+1")
    assert "records" in res and res["entityTypeName"] == "ApexClass"
    rules = sf.toolingexecute("query/?q=SELECT+ValidationName,Active+FROM+ValidationRule+WHERE+ValidationName+%3D+%27Budget_Not_Negative%27")
    assert rules["records"] == [
        {"attributes": rules["records"][0]["attributes"], "ValidationName": "Budget_Not_Negative", "Active": True}
    ]
    assert "/tooling/sobjects/ValidationRule/" in rules["records"][0]["attributes"]["url"]
    described = sf.toolingexecute("sobjects/ApexClass/describe")
    assert described["name"] == "ApexClass"
