//! Custom objects and fields from SFDX metadata and from the web UI's API.

use axum::Extension;
use localforce::config::Config;
use localforce::{AppState, Scheme, router};
use reqwest::Method;
use serde_json::{Value, json};

struct Org {
    base: String,
    http: reqwest::Client,
}

impl Org {
    async fn start(config: Config) -> Org {
        let state = AppState::new(config).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state).layer(Extension(Scheme("http")))).await });
        Org { base, http: reqwest::Client::new() }
    }

    async fn send(&self, method: Method, url: String, body: Option<Value>) -> (u16, Value) {
        let mut req = self.http.request(method, url).bearer_auth("any");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap() })
    }

    async fn rest(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        self.send(method, format!("{}/services/data/v62.0{path}", self.base), body).await
    }

    async fn dev(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        self.send(method, format!("{}/__localforce/api{path}", self.base), body).await
    }

    async fn create(&self, obj: &str, body: Value) -> String {
        let (s, v) = self.rest(Method::POST, &format!("/sobjects/{obj}"), Some(body)).await;
        assert_eq!(s, 201, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    async fn query(&self, soql: &str) -> Value {
        let q: String = url::form_urlencoded::byte_serialize(soql.as_bytes()).collect();
        let (s, v) = self.rest(Method::GET, &format!("/query?q={q}"), None).await;
        assert_eq!(s, 200, "{soql}: {v}");
        v
    }
}

fn error_code(v: &Value) -> &str {
    v[0]["errorCode"].as_str().unwrap_or("")
}

#[tokio::test]
async fn sfdx_metadata() {
    let mut config = Config::for_tests("").unwrap();
    config.metadata = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sfdx-project")];
    let org = Org::start(config).await;

    let acct = org.create("Account", json!({ "Name": "Acme", "Customer_Code__c": "ACME" })).await;
    // Auto-number name, picklist default, global value set.
    let inv = org
        .create("Invoice__c", json!({ "Account__c": acct, "Currency__c": "GBP", "Due_Date__c": "2020-01-01" }))
        .await;
    let (_, rec) = org.rest(Method::GET, &format!("/sobjects/Invoice__c/{inv}"), None).await;
    assert_eq!(rec["Name"], "INV-0001");
    assert_eq!(rec["Status__c"], "Draft");
    let (s, v) =
        org.rest(Method::POST, "/sobjects/Invoice__c", Some(json!({ "Account__c": acct, "Currency__c": "EUR" }))).await;
    assert_eq!((s, error_code(&v)), (400, "INVALID_OR_NULL_FOR_RESTRICTED_PICKLIST"));
    // Required lookup.
    let (s, v) = org.rest(Method::POST, "/sobjects/Invoice__c", Some(json!({}))).await;
    assert_eq!((s, error_code(&v)), (400, "REQUIRED_FIELD_MISSING"));
    // Formula and roll-up fields are read-only.
    let (s, v) =
        org.rest(Method::PATCH, &format!("/sobjects/Invoice__c/{inv}"), Some(json!({ "Is_Overdue__c": true }))).await;
    assert_eq!((s, error_code(&v)), (400, "INVALID_FIELD_FOR_INSERT_UPDATE"));

    // Master-detail children, queried through the child relationship.
    org.create("Invoice_Line__c", json!({ "Name": "Widgets", "Invoice__c": inv, "Amount__c": 40 })).await;
    org.create("Invoice_Line__c", json!({ "Name": "Gadgets", "Invoice__c": inv, "Amount__c": 2.5 })).await;
    let v = org.query("SELECT Name, Account__r.Customer_Code__c, (SELECT Amount__c FROM Lines__r ORDER BY Amount__c) FROM Invoice__c").await;
    assert_eq!(v["records"][0]["Account__r"]["Customer_Code__c"], "ACME");
    assert_eq!(v["records"][0]["Lines__r"]["records"][1]["Amount__c"], 40.0);
    // Account's child relationship from the metadata's <relationshipName>.
    let v = org.query("SELECT (SELECT Name FROM Invoices__r) FROM Account").await;
    assert_eq!(v["records"][0]["Invoices__r"]["totalSize"], 1);

    // deleteConstraint Restrict blocks deleting the account; deleting the
    // invoice cascades to its master-detail lines.
    let (s, v) = org.rest(Method::DELETE, &format!("/sobjects/Account/{acct}"), None).await;
    assert_eq!((s, error_code(&v)), (400, "DELETE_FAILED"));
    let (s, _) = org.rest(Method::DELETE, &format!("/sobjects/Invoice__c/{inv}"), None).await;
    assert_eq!(s, 204);
    assert_eq!(org.query("SELECT COUNT() FROM Invoice_Line__c").await["totalSize"], 0);
    let (s, _) = org.rest(Method::DELETE, &format!("/sobjects/Account/{acct}"), None).await;
    assert_eq!(s, 204);

    // Describe reflects the metadata.
    let (_, d) = org.rest(Method::GET, "/sobjects/Invoice__c/describe", None).await;
    let field = |n: &str| d["fields"].as_array().unwrap().iter().find(|f| f["name"] == n).unwrap().clone();
    assert_eq!(field("Name")["autoNumber"], true);
    assert_eq!(field("Is_Overdue__c")["calculated"], true);
    assert_eq!(field("Is_Overdue__c")["type"], "boolean");
    assert_eq!(field("Currency__c")["restrictedPicklist"], true);
    assert_eq!(d["label"], "Invoice");
    // Platform events aren't loaded.
    let (s, _) = org.rest(Method::GET, "/sobjects/Order_Event__e/describe", None).await;
    assert_eq!(s, 404);
    // Metadata definitions can't be changed from the UI.
    let (s, v) = org.dev(Method::DELETE, "/objects/Invoice__c/fields/Due_Date__c", None).await;
    assert_eq!(s, 400);
    assert!(v["error"].as_str().unwrap().contains("SFDX metadata"), "{v}");
}

#[tokio::test]
async fn schema_changes_from_the_ui() {
    let org = Org::start(Config::for_tests("").unwrap()).await;
    let (s, v) = org
        .dev(
            Method::POST,
            "/objects",
            Some(json!({ "name": "Project__c", "label": "Project", "name_field_format": "P-{000}" })),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    for field in [
        json!({ "name": "Account__c", "type": "reference", "reference_to": "Account", "child_relationship_name": "Projects__r" }),
        json!({ "name": "Stage__c", "type": "picklist", "values": ["Planned", "Active"], "restricted": true }),
        json!({ "name": "Budget__c", "type": "currency" }),
    ] {
        let (s, v) = org.dev(Method::POST, "/objects/Project__c/fields", Some(field)).await;
        assert_eq!(s, 201, "{v}");
    }
    let acct = org.create("Account", json!({ "Name": "Acme" })).await;
    let id = org.create("Project__c", json!({ "Account__c": acct, "Stage__c": "Active", "Budget__c": 10 })).await;
    assert!(id.starts_with('a'));
    let v = org.query("SELECT Name, (SELECT Name, Account__r.Name FROM Projects__r) FROM Account").await;
    assert_eq!(v["records"][0]["Projects__r"]["records"][0]["Name"], "P-001");

    // Editing a field: add a picklist value.
    let (s, v) = org
        .dev(
            Method::PUT,
            "/objects/Project__c/fields/Stage__c",
            Some(json!({ "name": "Stage__c", "type": "picklist", "values": ["Planned", "Active", "Done"], "restricted": true })),
        )
        .await;
    assert_eq!(s, 204, "{v}");
    let (s, _) =
        org.rest(Method::PATCH, &format!("/sobjects/Project__c/{id}"), Some(json!({ "Stage__c": "Done" }))).await;
    assert_eq!(s, 204);
    // A field's type can't change.
    let (s, _) = org
        .dev(
            Method::PUT,
            "/objects/Project__c/fields/Budget__c",
            Some(json!({ "name": "Budget__c", "type": "string" })),
        )
        .await;
    assert_eq!(s, 400);

    // Mistakes are refused with a reason.
    for (path, body) in [
        ("/objects", json!({ "name": "Project__c" })),
        ("/objects", json!({ "name": "No Suffix" })),
        ("/objects/Project__c/fields", json!({ "name": "Budget__c", "type": "double" })),
        ("/objects/Project__c/fields", json!({ "name": "Thing__c", "type": "reference", "reference_to": "Nope__c" })),
        // A second relationship named Projects__r on Account.
        (
            "/objects/Project__c/fields",
            json!({ "name": "Other__c", "type": "reference", "reference_to": "Account", "child_relationship_name": "Projects__r" }),
        ),
    ] {
        let (s, v) = org.dev(Method::POST, path, Some(body.clone())).await;
        assert_eq!(s, 400, "{body} should be refused: {v}");
    }
    let (s, _) = org.dev(Method::DELETE, "/objects/Project__c/fields/Name", None).await;
    assert_eq!(s, 400);
    let (s, _) = org.dev(Method::DELETE, "/objects/Account", None).await;
    assert_eq!(s, 400);

    // Deleting a field drops its data; deleting the object removes it from the API.
    let (s, _) = org.dev(Method::DELETE, "/objects/Project__c/fields/Budget__c", None).await;
    assert_eq!(s, 204);
    let (s, v) = org.rest(Method::GET, "/query?q=SELECT+Budget__c+FROM+Project__c", None).await;
    assert_eq!((s, error_code(&v)), (400, "INVALID_FIELD"));
    let (s, _) = org.dev(Method::DELETE, "/objects/Project__c", None).await;
    assert_eq!(s, 204);
    let (s, v) = org.rest(Method::GET, "/query?q=SELECT+Id+FROM+Project__c", None).await;
    assert_eq!((s, error_code(&v)), (400, "INVALID_TYPE"));
}

#[tokio::test]
async fn ui_records_api() {
    let org = Org::start(Config::for_tests("").unwrap()).await;
    let acct = org.create("Account", json!({ "Name": "Acme" })).await;
    for i in 0..2105 {
        // Over SOQL's 2,000-row OFFSET cap, which the UI's paging doesn't have.
        if i % 200 == 0 {
            let recs: Vec<Value> = (i..(i + 200).min(2105))
                .map(|n| json!({ "attributes": { "type": "Contact" }, "LastName": format!("C{n:04}"), "AccountId": acct }))
                .collect();
            let (s, _) = org.rest(Method::POST, "/composite/sobjects", Some(json!({ "records": recs }))).await;
            assert_eq!(s, 200);
        }
    }
    let (s, v) = org
        .dev(Method::GET, "/records/Contact?fields=Name,Account.Name&order=Name&dir=asc&limit=10&offset=2100", None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["totalSize"], 2105);
    assert_eq!(v["records"][0]["Name"], "C2100");
    assert_eq!(v["records"][0]["Account"]["Name"], "Acme");
    let (_, v) = org.dev(Method::GET, "/records/Contact?q=c000&limit=50", None).await;
    assert_eq!(v["totalSize"], 10);
    let (_, v) = org.dev(Method::GET, &format!("/records/Account/{acct}/related"), None).await;
    let contacts = v.as_array().unwrap().iter().find(|r| r["relationship"] == "Contacts").unwrap();
    assert_eq!(contacts["count"], 2105);

    // Recycle bin and restore, including cascaded children.
    org.rest(Method::DELETE, &format!("/sobjects/Account/{acct}"), None).await;
    let (_, v) = org.dev(Method::GET, "/records/Account?deleted=1", None).await;
    assert_eq!(v["totalSize"], 1);
    let (s, _) = org.dev(Method::POST, &format!("/records/Account/{acct}/undelete"), None).await;
    assert_eq!(s, 204);
    assert_eq!(org.query("SELECT COUNT() FROM Contact").await["totalSize"], 2105);

    // The request log saw the REST calls, and reset empties the org.
    let (_, log) = org.dev(Method::GET, "/requests", None).await;
    assert!(log.as_array().unwrap().iter().any(|e| e["path"].as_str().unwrap().contains("/composite/sobjects")));
    let (s, _) = org.dev(Method::POST, "/reset", None).await;
    assert_eq!(s, 204);
    assert_eq!(org.query("SELECT COUNT() FROM Contact").await["totalSize"], 0);
    // Cross-origin requests to the UI API are refused.
    let resp = org
        .http
        .post(format!("{}/__localforce/api/reset", org.base))
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
}

#[tokio::test]
async fn ui_pages_and_assets() {
    let org = Org::start(Config::for_tests("").unwrap()).await;
    let get = |path: &str| org.http.get(format!("{}{path}", org.base)).send();
    // The app's own paths all serve its page, which routes in the browser.
    for path in [
        "/__localforce/ui/",
        "/__localforce/ui/data/Account",
        "/__localforce/ui/data/Account/fields",
        "/__localforce/ui/log/3",
    ] {
        let resp = get(path).await.unwrap();
        assert_eq!(resp.status().as_u16(), 200, "{path}");
        assert_eq!(resp.headers()["cache-control"], "no-cache", "{path}");
        assert!(resp.text().await.unwrap().contains(r#"<div id="root"></div>"#), "{path}");
    }
    // So do the script, stylesheet and fonts it loads, which never change.
    let page = get("/__localforce/ui/").await.unwrap().text().await.unwrap();
    let assets: Vec<&str> = page.split('"').filter(|s| s.starts_with("/__localforce/ui/assets/")).collect();
    assert!(assets.iter().any(|a| a.ends_with(".js")) && assets.iter().any(|a| a.ends_with(".css")), "{page}");
    for asset in assets {
        let resp = get(asset).await.unwrap();
        assert_eq!(resp.status().as_u16(), 200, "{asset}");
        assert!(resp.headers()["cache-control"].to_str().unwrap().contains("immutable"), "{asset}");
    }
    assert_eq!(get("/__localforce/ui/favicon.svg").await.unwrap().status().as_u16(), 200);
    // A missing file isn't the page.
    for path in ["/__localforce/ui/assets/missing.js", "/__localforce/ui/missing.js"] {
        assert_eq!(get(path).await.unwrap().status().as_u16(), 404, "{path}");
    }
}
