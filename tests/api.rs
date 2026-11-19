//! End-to-end tests over HTTP against an in-memory org.

use axum::Extension;
use localforce::config::Config;
use localforce::{AppState, Scheme, router};
use serde_json::{Value, json};

const SCHEMA: &str = r#"
[[fields]]
object = "Account"
name = "Region__c"
type = "multipicklist"
values = ["EMEA", "AMER", "APAC"]

[[objects]]
name = "Invoice__c"
[[objects.fields]]
name = "Account__c"
type = "reference"
reference_to = "Account"
child_relationship_name = "Invoices__r"
master_detail = true
[[objects.fields]]
name = "Amount__c"
type = "currency"

[[objects]]
name = "Zone__c"
[[objects.fields]]
name = "Rate__c"
type = "double"

[[objects]]
name = "Lab__c"
[[objects.fields]]
name = "Zone_Name__c"
type = "string"
[[objects.fields]]
name = "Text__c"
type = "string"
[[objects.fields]]
name = "N__c"
type = "double"
[[objects.fields]]
name = "D__c"
type = "date"
[[objects.fields]]
name = "At__c"
type = "datetime"
[[objects.fields]]
name = "Due__c"
type = "date"
default_formula = "TODAY() + 30"
[[objects.fields]]
name = "Who__c"
type = "string"
default_formula = "$User.Username & ' @ ' & $Organization.Name"
[[objects.fields]]
name = "Text_Fns__c"
type = "string"
formula = "INITCAP(Text__c) & '|' & REVERSE(Text__c) & '|' & HTMLENCODE(Text__c) & '|' & URLENCODE(Text__c) & '|' & IF(ISNUMBER(Text__c), 'num', 'text')"
[[objects.fields]]
name = "Math_Fns__c"
type = "string"
formula = "TEXT(FLOOR(N__c)) & ',' & TEXT(CEILING(N__c)) & ',' & TEXT(MFLOOR(N__c)) & ',' & TEXT(MCEILING(N__c)) & ',' & TEXT(ROUND(N__c * 1000, -2))"
[[objects.fields]]
name = "Date_Fns__c"
type = "string"
formula = "TEXT(DAYOFYEAR(D__c)) & ',' & TEXT(ISOWEEK(D__c)) & ',' & TEXT(ISOYEAR(D__c)) & ',' & TEXT(UNIXTIMESTAMP(D__c))"
[[objects.fields]]
name = "Time_Fns__c"
type = "string"
formula = "TEXT(HOUR(TIMEVALUE(At__c))) & ':' & TEXT(MINUTE(TIMEVALUE(At__c))) & ',' & TEXT(TIMEVALUE(At__c) + 3600000) & ',' & TEXT(FROMUNIXTIME(UNIXTIMESTAMP(At__c) + 60))"
[[objects.fields]]
name = "Miles__c"
type = "double"
scale = 0
formula = "DISTANCE(GEOLOCATION(51.5074, -0.1278), GEOLOCATION(48.8566, 2.3522), 'mi')"
[[objects.fields]]
name = "Org__c"
type = "string"
formula = "$Organization.Id & ' ' & $Profile.Name & ' ' & TEXT(LEN(CASESAFEID(Id)))"

[[validation_rules]]
object = "Lab__c"
name = "Known_Zone"
condition = "NOT(ISBLANK(Zone_Name__c)) && ISBLANK(VLOOKUP($ObjectType.Zone__c.Fields.Name, $ObjectType.Zone__c.Fields.Name, Zone_Name__c))"
message = "No such zone."
field = "Zone_Name__c"
"#;

struct Org {
    base: String,
    http: reqwest::Client,
}

impl Org {
    async fn start() -> Org {
        let state = AppState::new(Config::for_tests(SCHEMA).unwrap()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state).layer(Extension(Scheme("http")))).await });
        Org { base, http: reqwest::Client::new() }
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self.http.request(method, format!("{}/services/data/v62.0{path}", self.base)).bearer_auth("any");
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap() })
    }

    async fn create(&self, obj: &str, body: Value) -> String {
        let (s, v) = self.call(reqwest::Method::POST, &format!("/sobjects/{obj}"), Some(body)).await;
        assert_eq!(s, 201, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    async fn query(&self, soql: &str) -> Value {
        let (s, v) = self.call(reqwest::Method::GET, &format!("/query?q={}", urlencode(soql)), None).await;
        assert_eq!(s, 200, "{soql}: {v}");
        v
    }

    async fn query_err(&self, soql: &str) -> String {
        let (s, v) = self.call(reqwest::Method::GET, &format!("/query?q={}", urlencode(soql)), None).await;
        assert_eq!(s, 400, "{soql}: {v}");
        v[0]["errorCode"].as_str().unwrap().to_string()
    }
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn names(v: &Value) -> Vec<String> {
    v["records"].as_array().unwrap().iter().map(|r| r["Name"].as_str().unwrap_or("<null>").to_string()).collect()
}

#[tokio::test]
async fn soql_semantics() {
    let org = Org::start().await;
    let a = org.create("Account", json!({ "Name": "Acme", "Industry": "Technology", "Region__c": "EMEA;APAC" })).await;
    org.create("Account", json!({ "Name": "beta", "Region__c": "AMER" })).await;
    org.create("Account", json!({ "Name": "Gamma", "Industry": "Retail" })).await;
    org.create("Invoice__c", json!({ "Name": "I1", "Account__c": a, "Amount__c": 10 })).await;
    org.create("Invoice__c", json!({ "Name": "I2", "Account__c": &a[..15], "Amount__c": 5.5 })).await;

    // Case-insensitive equality and ordering; ids in either form.
    assert_eq!(names(&org.query("SELECT Name FROM Account WHERE Name = 'ACME'").await), ["Acme"]);
    assert_eq!(names(&org.query("SELECT Name FROM Account ORDER BY Name").await), ["Acme", "beta", "Gamma"]);
    assert_eq!(names(&org.query(&format!("SELECT Name FROM Account WHERE Id = '{}'", &a[..15])).await), ["Acme"]);
    // != and NOT IN match nulls; < doesn't.
    assert_eq!(
        names(&org.query("SELECT Name FROM Account WHERE Industry != 'Retail' ORDER BY Name").await),
        ["Acme", "beta"]
    );
    assert_eq!(
        names(&org.query("SELECT Name FROM Account WHERE NOT Industry IN ('Retail') ORDER BY Name").await),
        ["Acme", "beta"]
    );
    // Multi-select picklists.
    assert_eq!(names(&org.query("SELECT Name FROM Account WHERE Region__c INCLUDES ('apac;emea')").await), ["Acme"]);
    assert_eq!(
        names(&org.query("SELECT Name FROM Account WHERE Region__c EXCLUDES ('EMEA') ORDER BY Name").await),
        ["beta", "Gamma"]
    );
    // Nulls sort first by default, in both directions.
    assert_eq!(names(&org.query("SELECT Name FROM Account ORDER BY Industry DESC").await)[0], "beta");
    // Date literals against datetime fields.
    assert_eq!(org.query("SELECT COUNT() FROM Account WHERE CreatedDate = TODAY").await["totalSize"], 3);
    assert_eq!(org.query("SELECT COUNT() FROM Account WHERE CreatedDate < LAST_N_DAYS:7").await["totalSize"], 0);
    // Child subqueries and aggregates.
    let v = org
        .query(
            "SELECT Name, (SELECT Name FROM Invoices__r ORDER BY Name DESC LIMIT 1) FROM Account WHERE Name = 'Acme'",
        )
        .await;
    assert_eq!(v["records"][0]["Invoices__r"]["records"][0]["Name"], "I2");
    let v = org.query("SELECT SUM(Amount__c), MAX(Amount__c) biggest FROM Invoice__c").await;
    assert_eq!(v["records"][0]["expr0"], 15.5);
    assert_eq!(v["records"][0]["biggest"], 10.0);

    // Errors.
    assert_eq!(org.query_err("SELECT Name FROM Account WHERE Industry = 5").await, "INVALID_QUERY_FILTER_OPERATOR");
    assert_eq!(org.query_err("SELECT Name, COUNT(Id) FROM Account").await, "MALFORMED_QUERY");
    assert_eq!(org.query_err("SELECT FIELDS(ALL) FROM Account").await, "MALFORMED_QUERY");
    assert_eq!(org.query_err("SELECT Name FROM Account LIMIT 10 OFFSET 2001").await, "NUMBER_OUTSIDE_VALID_RANGE");
}

#[tokio::test]
async fn dml_rules() {
    let org = Org::start().await;
    let (s, v) = org.call(reqwest::Method::POST, "/sobjects/Contact", Some(json!({ "FirstName": "x" }))).await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (400, Some("REQUIRED_FIELD_MISSING")));
    let (s, v) = org
        .call(reqwest::Method::POST, "/sobjects/Account", Some(json!({ "Name": "x", "CreatedDate": "2020-01-01" })))
        .await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (400, Some("INVALID_FIELD_FOR_INSERT_UPDATE")));
    let (s, v) = org
        .call(
            reqwest::Method::POST,
            "/sobjects/Contact",
            Some(json!({ "LastName": "x", "AccountId": "001000000000000" })),
        )
        .await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (400, Some("INVALID_CROSS_REFERENCE_KEY")));

    let c = org.create("Contact", json!({ "FirstName": "Ada", "LastName": "Lovelace" })).await;
    let (_, rec) = org.call(reqwest::Method::GET, &format!("/sobjects/Contact/{c}"), None).await;
    assert_eq!(rec["Name"], "Ada Lovelace");
    assert_eq!(rec["OwnerId"].as_str().unwrap().get(..3), Some("005"));
    org.call(reqwest::Method::PATCH, &format!("/sobjects/Contact/{c}"), Some(json!({ "FirstName": "Augusta" }))).await;
    let (_, rec) = org.call(reqwest::Method::GET, &format!("/sobjects/Contact/{c}?fields=Name"), None).await;
    assert_eq!(rec["Name"], "Augusta Lovelace");

    let (s, _) = org.call(reqwest::Method::DELETE, &format!("/sobjects/Contact/{c}"), None).await;
    assert_eq!(s, 204);
    let (s, v) = org.call(reqwest::Method::GET, &format!("/sobjects/Contact/{c}"), None).await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (404, Some("ENTITY_IS_DELETED")));
    let (_, v) =
        org.call(reqwest::Method::GET, &format!("/queryAll?q={}", urlencode("SELECT Name FROM Contact")), None).await;
    assert_eq!(v["totalSize"], 1);
}

#[tokio::test]
async fn sessions() {
    let org = Org::start().await;
    let resp = org.http.get(format!("{}/services/data/v62.0/sobjects", org.base)).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    let token: Value = org
        .http
        .post(format!("{}/services/oauth2/token", org.base))
        .form(&[("grant_type", "password"), ("username", "admin@localforce.dev"), ("password", "x")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let t = token["access_token"].as_str().unwrap();
    assert!(t.starts_with("00D"));
    assert_eq!(token["instance_url"], org.base);
    org.http.post(format!("{}/services/oauth2/revoke", org.base)).form(&[("token", t)]).send().await.unwrap();
    let resp = org.http.get(format!("{}/services/data/v62.0/sobjects", org.base)).bearer_auth(t).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn formula_functions_and_defaults() {
    let org = Org::start().await;
    let get = |id: &str| format!("/sobjects/Lab__c/{id}");
    let id = org
        .create(
            "Lab__c",
            json!({"Name": "lab", "Text__c": "hello wORLD <&>", "N__c": -2.5, "D__c": "2024-12-30", "At__c": "2024-03-10T14:30:00Z"}),
        )
        .await;
    let (_, r) = org.call(reqwest::Method::GET, &get(&id), None).await;
    assert_eq!(
        r["Text_Fns__c"],
        "Hello World <&>|>&< DLROw olleh|hello wORLD &lt;&amp;&gt;|hello%20wORLD%20%3C%26%3E|text"
    );
    assert_eq!(r["Math_Fns__c"], "-2,-3,-3,-2,-2500");
    assert_eq!(r["Date_Fns__c"], "365,1,2025,1735516800");
    assert_eq!(r["Time_Fns__c"], "14:30,15:30:00.000Z,2024-03-10 14:31:00Z");
    assert_eq!(r["Miles__c"], 213.0);
    let org_text = r["Org__c"].as_str().unwrap();
    assert!(org_text.starts_with("00D") && org_text.ends_with(" System Administrator 18"), "{org_text}");

    // Default formulas: evaluated on create when the field isn't given.
    let due = chrono::Utc::now().date_naive() + chrono::Days::new(30);
    assert_eq!(r["Due__c"], due.format("%Y-%m-%d").to_string());
    assert!(r["Who__c"].as_str().unwrap().ends_with(" @ localforce"), "{}", r["Who__c"]);
    let given = org.create("Lab__c", json!({"Name": "given", "Due__c": "2030-01-01", "Who__c": null})).await;
    let (_, r) = org.call(reqwest::Method::GET, &get(&given), None).await;
    assert_eq!(r["Due__c"], "2030-01-01");
    assert_eq!(r["Who__c"], Value::Null);
    let (_, d) = org.call(reqwest::Method::GET, "/sobjects/Lab__c/describe", None).await;
    let due_field = d["fields"].as_array().unwrap().iter().find(|f| f["name"] == "Due__c").unwrap();
    assert_eq!(due_field["defaultValueFormula"], "TODAY() + 30");
    assert_eq!(due_field["defaultedOnCreate"], true);

    // VLOOKUP in a validation rule.
    let (s, v) =
        org.call(reqwest::Method::POST, "/sobjects/Lab__c", Some(json!({"Name": "x", "Zone_Name__c": "North"}))).await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (400, Some("FIELD_CUSTOM_VALIDATION_EXCEPTION")), "{v}");
    org.create("Zone__c", json!({"Name": "North"})).await;
    org.create("Lab__c", json!({"Name": "y", "Zone_Name__c": "North"})).await;

    // Formula, fractional-number and long text fields can't be grouped; long text can't be filtered or sorted.
    for soql in [
        "SELECT Math_Fns__c, COUNT(Id) FROM Lab__c GROUP BY Math_Fns__c",
        "SELECT N__c, COUNT(Id) FROM Lab__c GROUP BY N__c",
        "SELECT Id FROM Account WHERE Description = 'x'",
        "SELECT Id FROM Account ORDER BY Description",
    ] {
        assert_eq!(org.query_err(soql).await, "INVALID_FIELD", "{soql}");
    }
    org.query("SELECT Id FROM Lab__c WHERE Math_Fns__c != null ORDER BY Math_Fns__c").await;
    org.query("SELECT Zone_Name__c, COUNT(Id) FROM Lab__c GROUP BY Zone_Name__c").await;
}

#[tokio::test]
async fn search_matches_whole_words() {
    let org = Org::start().await;
    let id = org.create("Account", json!({"Name": "Searchable Industries", "Phone": "+1 (555) 123-4567"})).await;
    let c = org.create("Contact", json!({"LastName": "Smith", "Email": "john.smith@example.com"})).await;
    let search = |q: &str| {
        let q = format!("FIND {{{q}}} RETURNING Account(Id), Contact(Id)");
        let path = format!("/search?q={}", urlencode(&q));
        let org = &org;
        async move {
            let (s, v) = org.call(reqwest::Method::GET, &path, None).await;
            assert_eq!(s, 200, "{q}: {v}");
            v["searchRecords"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["Id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(search("Searchable").await, std::slice::from_ref(&id));
    assert!(search("Searchab").await.is_empty());
    assert!(search("earchable").await.is_empty());
    assert_eq!(search("Search*").await, std::slice::from_ref(&id));
    for phone in ["4567", "123-4567", "5551234567", "15551234567"] {
        assert_eq!(search(phone).await, std::slice::from_ref(&id), "{phone}");
    }
    assert!(search("567").await.is_empty());
    for email in ["john.smith@example.com", "johnsmith", "smith", "example.com"] {
        assert_eq!(search(email).await, std::slice::from_ref(&c), "{email}");
    }
}

#[tokio::test]
async fn sales_objects() {
    use reqwest::Method;
    let org = Org::start().await;
    let get = |obj: &'static str, id: String| {
        let org = &org;
        async move { org.call(Method::GET, &format!("/sobjects/{obj}/{id}"), None).await.1 }
    };
    let std_book = org.query("SELECT Id FROM Pricebook2 WHERE IsStandard = true").await["records"][0]["Id"]
        .as_str()
        .unwrap()
        .to_string();
    let prod = org.create("Product2", json!({"Name": "Widget", "ProductCode": "W-1", "IsActive": true})).await;
    let entry = org
        .create(
            "PricebookEntry",
            json!({"Pricebook2Id": std_book, "Product2Id": prod, "UnitPrice": 10, "IsActive": true}),
        )
        .await;
    let acct = org.create("Account", json!({"Name": "Acme"})).await;
    let camp = org.create("Campaign", json!({"Name": "Spring"})).await;
    let opp = org
        .create(
            "Opportunity",
            json!({"Name": "Big deal", "AccountId": acct, "StageName": "Prospecting", "CloseDate": "2026-12-01", "Amount": 5, "CampaignId": camp}),
        )
        .await;

    // Opportunity products: prices, names, the opportunity's price book and amount.
    let (s, v) = org
        .call(
            Method::POST,
            "/sobjects/OpportunityLineItem",
            Some(json!({"OpportunityId": opp, "PricebookEntryId": entry, "Quantity": 2})),
        )
        .await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (400, Some("FIELD_INTEGRITY_EXCEPTION")), "{v}");
    let (s, v) = org
        .call(
            Method::POST,
            "/sobjects/OpportunityLineItem",
            Some(json!({"OpportunityId": opp, "PricebookEntryId": entry, "Quantity": 2, "UnitPrice": 1, "TotalPrice": 2})),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    assert!(v[0]["message"].as_str().unwrap().contains("only one of unit price or total price"));
    let (s, v) = org
        .call(
            Method::POST,
            "/sobjects/OpportunityLineItem",
            Some(json!({"OpportunityId": opp, "Quantity": 1, "UnitPrice": 1})),
        )
        .await;
    assert_eq!((s, v[0]["fields"][0].as_str()), (400, Some("PricebookEntryId")), "{v}");
    let line = org
        .create(
            "OpportunityLineItem",
            json!({"OpportunityId": opp, "PricebookEntryId": entry, "Quantity": 3, "UnitPrice": 10, "Discount": 10}),
        )
        .await;
    let l = get("OpportunityLineItem", line.clone()).await;
    assert_eq!(l["Name"], "Big deal Widget");
    assert_eq!(
        (l["ListPrice"].as_f64(), l["Subtotal"].as_f64(), l["TotalPrice"].as_f64()),
        (Some(10.0), Some(30.0), Some(27.0))
    );
    assert_eq!(l["Product2Id"], json!(prod));
    org.create(
        "OpportunityLineItem",
        json!({"OpportunityId": opp, "PricebookEntryId": entry, "Quantity": 4, "TotalPrice": 100}),
    )
    .await;
    let o = get("Opportunity", opp.clone()).await;
    assert_eq!(o["Amount"].as_f64(), Some(127.0));
    assert_eq!(o["Pricebook2Id"], json!(std_book));
    assert_eq!(
        (o["HasOpportunityLineItem"].as_bool(), o["TotalOpportunityQuantity"].as_f64()),
        (Some(true), Some(7.0))
    );
    assert_eq!(o["ExpectedRevenue"].as_f64(), Some(12.7));
    // The amount follows the products, not what's written to it.
    let (s, _) = org.call(Method::PATCH, &format!("/sobjects/Opportunity/{opp}"), Some(json!({"Amount": 1}))).await;
    assert_eq!(s, 204);
    assert_eq!(get("Opportunity", opp.clone()).await["Amount"].as_f64(), Some(127.0));
    let (s, _) =
        org.call(Method::PATCH, &format!("/sobjects/OpportunityLineItem/{line}"), Some(json!({"Quantity": 1}))).await;
    assert_eq!(s, 204);
    // A new quantity keeps the total and changes the unit price; a new discount keeps the unit price.
    let l = get("OpportunityLineItem", line.clone()).await;
    assert_eq!((l["UnitPrice"].as_f64(), l["TotalPrice"].as_f64()), (Some(30.0), Some(27.0)));
    let (s, _) =
        org.call(Method::PATCH, &format!("/sobjects/OpportunityLineItem/{line}"), Some(json!({"Discount": 0}))).await;
    assert_eq!(s, 204);
    let l = get("OpportunityLineItem", line.clone()).await;
    assert_eq!((l["UnitPrice"].as_f64(), l["TotalPrice"].as_f64()), (Some(30.0), Some(30.0)));
    assert_eq!(get("Opportunity", opp.clone()).await["Amount"].as_f64(), Some(130.0));
    let other_book = org.create("Pricebook2", json!({"Name": "Partner", "IsActive": true})).await;
    let (s, v) = org
        .call(Method::PATCH, &format!("/sobjects/Opportunity/{opp}"), Some(json!({"Pricebook2Id": other_book})))
        .await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = org.call(Method::DELETE, &format!("/sobjects/PricebookEntry/{entry}"), None).await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (400, Some("DELETE_FAILED")), "{v}");
    let (s, _) = org.call(Method::DELETE, &format!("/sobjects/OpportunityLineItem/{line}"), None).await;
    assert_eq!(s, 204);
    assert_eq!(get("Opportunity", opp.clone()).await["Amount"].as_f64(), Some(100.0));
    let q = org
        .query(&format!("SELECT Name, (SELECT Quantity FROM OpportunityLineItems) FROM Opportunity WHERE Id = '{opp}'"))
        .await;
    assert_eq!(q["records"][0]["OpportunityLineItems"]["totalSize"], 1);

    // Campaign members and the campaign's counts.
    let lead = org.create("Lead", json!({"LastName": "Lovelace", "FirstName": "Ada", "Company": "Engines"})).await;
    let contact = org.create("Contact", json!({"LastName": "Babbage", "AccountId": acct})).await;
    org.create("CampaignMember", json!({"CampaignId": camp, "LeadId": lead})).await;
    let (s, v) =
        org.call(Method::POST, "/sobjects/CampaignMember", Some(json!({"CampaignId": camp, "LeadId": lead}))).await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (400, Some("DUPLICATE_VALUE")), "{v}");
    let m =
        org.create("CampaignMember", json!({"CampaignId": camp, "ContactId": contact, "Status": "Responded"})).await;
    assert_eq!(get("CampaignMember", m).await["HasResponded"], true);
    let c = get("Campaign", camp.clone()).await;
    assert_eq!(
        (c["NumberOfLeads"].as_i64(), c["NumberOfContacts"].as_i64(), c["NumberOfResponses"].as_i64()),
        (Some(1), Some(1), Some(1))
    );
    assert_eq!((c["NumberOfOpportunities"].as_i64(), c["AmountAllOpportunities"].as_f64()), (Some(1), Some(100.0)));

    // Contact roles: one primary per opportunity.
    let r1 = org
        .create("OpportunityContactRole", json!({"OpportunityId": opp, "ContactId": contact, "IsPrimary": true}))
        .await;
    let c2 = org.create("Contact", json!({"LastName": "Hopper"})).await;
    org.create(
        "OpportunityContactRole",
        json!({"OpportunityId": opp, "ContactId": c2, "IsPrimary": true, "Role": "Evaluator"}),
    )
    .await;
    assert_eq!(get("OpportunityContactRole", r1).await["IsPrimary"], false);

    // Contracts: end date and activation.
    let k = org.create("Contract", json!({"AccountId": acct, "StartDate": "2026-01-15", "ContractTerm": 12})).await;
    let kv = get("Contract", k.clone()).await;
    assert_eq!(
        (kv["EndDate"].as_str(), kv["Status"].as_str(), kv["StatusCode"].as_str()),
        (Some("2027-01-14"), Some("Draft"), Some("Draft"))
    );
    org.call(Method::PATCH, &format!("/sobjects/Contract/{k}"), Some(json!({"Status": "Activated"}))).await;
    assert!(get("Contract", k).await["ActivatedDate"].is_string());

    // Orders: a price book first, products, the total, activation.
    let order = org.create("Order", json!({"AccountId": acct, "EffectiveDate": "2026-02-01"})).await;
    let (s, _) = org
        .call(
            Method::POST,
            "/sobjects/OrderItem",
            Some(json!({"OrderId": order, "PricebookEntryId": entry, "Quantity": 1, "UnitPrice": 10})),
        )
        .await;
    assert_eq!(s, 400);
    let (s, v) =
        org.call(Method::PATCH, &format!("/sobjects/Order/{order}"), Some(json!({"Status": "Activated"}))).await;
    assert_eq!((s, v[0]["message"].as_str()), (400, Some("An order must have at least one product.")), "{v}");
    let (s, v) = org
        .call(
            Method::POST,
            "/sobjects/Order",
            Some(json!({"AccountId": acct, "EffectiveDate": "2026-02-01", "Status": "Activated"})),
        )
        .await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (400, Some("FAILED_ACTIVATION")), "{v}");
    org.call(Method::PATCH, &format!("/sobjects/Order/{order}"), Some(json!({"Pricebook2Id": std_book}))).await;
    let item = org
        .create("OrderItem", json!({"OrderId": order, "PricebookEntryId": entry, "Quantity": 3, "UnitPrice": 9.5}))
        .await;
    assert_eq!(get("Order", order.clone()).await["TotalAmount"].as_f64(), Some(28.5));
    let (s, v) =
        org.call(Method::PATCH, &format!("/sobjects/Order/{order}"), Some(json!({"Status": "Activated"}))).await;
    assert_eq!(s, 204, "{v}");
    let (s, _) = org
        .call(
            Method::POST,
            "/sobjects/OrderItem",
            Some(json!({"OrderId": order, "PricebookEntryId": entry, "Quantity": 1, "UnitPrice": 10})),
        )
        .await;
    assert_eq!(s, 400);
    // An activated order's products can be edited, but not removed.
    let (s, v) = org.call(Method::PATCH, &format!("/sobjects/OrderItem/{item}"), Some(json!({"Quantity": 4}))).await;
    assert_eq!(s, 204, "{v}");
    assert_eq!(get("Order", order.clone()).await["TotalAmount"].as_f64(), Some(38.0));
    let (s, v) = org.call(Method::DELETE, &format!("/sobjects/OrderItem/{item}"), None).await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (400, Some("ENTITY_IS_LOCKED")), "{v}");

    // Assets need an account or a contact.
    let (s, v) = org.call(Method::POST, "/sobjects/Asset", Some(json!({"Name": "Loose"}))).await;
    assert_eq!((s, v[0]["message"].as_str()), (400, Some("Every asset needs an account, a contact, or both.")), "{v}");
    org.create("Asset", json!({"Name": "Unit 1", "ContactId": contact, "Product2Id": prod})).await;

    // Deleting the opportunity takes its products with it, and the campaign forgets it.
    org.call(Method::DELETE, &format!("/sobjects/Opportunity/{opp}"), None).await;
    let c = get("Campaign", camp.clone()).await;
    assert_eq!(c["NumberOfOpportunities"].as_i64(), Some(0));
    let n = org.query(&format!("SELECT COUNT() FROM OpportunityLineItem WHERE OpportunityId = '{opp}'")).await;
    assert_eq!(n["totalSize"], 0);
}

#[tokio::test]
async fn oauth_flows() {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
    let org = Org::start().await;
    let no_redirects = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let token = |form: Vec<(&'static str, String)>| {
        let org = &org;
        async move {
            let resp = org.http.post(format!("{}/services/oauth2/token", org.base)).form(&form).send().await.unwrap();
            (resp.status().as_u16(), resp.json::<Value>().await.unwrap())
        }
    };

    // The picker lists the users.
    let page = no_redirects
        .get(format!(
            "{}/services/oauth2/authorize?response_type=code&client_id=app&redirect_uri=http://localhost:3000/cb",
            org.base
        ))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("admin@localforce.dev"), "{page}");

    // Authorization code with PKCE, chosen in the picker.
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = B64.encode(<sha2::Sha256 as sha2::Digest>::digest(verifier.as_bytes()));
    let user = org.query("SELECT Id FROM User LIMIT 1").await["records"][0]["Id"].as_str().unwrap().to_string();
    let resp = no_redirects
        .post(format!("{}/services/oauth2/authorize", org.base))
        .form(&[
            ("response_type", "code"),
            ("client_id", "app"),
            ("redirect_uri", "http://localhost:3000/cb"),
            ("state", "xyz"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("lf_user", user.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 303);
    let location = url::Url::parse(resp.headers()["location"].to_str().unwrap()).unwrap();
    let q: std::collections::HashMap<_, _> = location.query_pairs().into_owned().collect();
    assert_eq!(q["state"], "xyz");
    let code = q["code"].clone();
    let exchange = |code: String, verifier: &'static str| {
        vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code),
            ("redirect_uri", "http://localhost:3000/cb".into()),
            ("code_verifier", verifier.into()),
        ]
    };
    let (s, v) = token(exchange(code.clone(), "wrong")).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("invalid_grant")));
    // A failed exchange uses the code up.
    let (s, _) = token(exchange(code, verifier)).await;
    assert_eq!(s, 400);

    let location = no_redirects
        .get(format!(
            "{}/services/oauth2/authorize?response_type=code&client_id=app&redirect_uri=http://localhost:3000/cb&login_hint=admin@localforce.dev&code_challenge={challenge}",
            org.base
        ))
        .send()
        .await
        .unwrap()
        .headers()["location"]
        .to_str()
        .unwrap()
        .to_string();
    let code = url::Url::parse(&location).unwrap().query_pairs().find(|(k, _)| k == "code").unwrap().1.into_owned();
    let (s, v) = token(exchange(code, verifier)).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["id"].as_str().unwrap().rsplit('/').next(), Some(user.as_str()));
    let refresh = v["refresh_token"].as_str().unwrap().to_string();
    let (s, v) = token(vec![("grant_type", "refresh_token".into()), ("refresh_token", refresh)]).await;
    assert_eq!(s, 200, "{v}");
    assert!(v.get("refresh_token").is_none());

    // The implicit flow puts the token in the fragment.
    let location = no_redirects
        .get(format!(
            "{}/services/oauth2/authorize?response_type=token&client_id=app&redirect_uri=http://localhost:3000/cb&login_hint=admin@localforce.dev&state=s1",
            org.base
        ))
        .send()
        .await
        .unwrap()
        .headers()["location"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.starts_with("http://localhost:3000/cb#access_token=00D"), "{location}");
    assert!(location.ends_with("state=s1"));

    // JWT bearer: claims are checked, the signature isn't.
    let jwt = |claims: Value| format!("{}.{}.sig", B64.encode(br#"{"alg":"RS256"}"#), B64.encode(claims.to_string()));
    let exp = chrono::Utc::now().timestamp() + 180;
    let grant =
        |a: String| vec![("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer".to_string()), ("assertion", a)];
    let (s, v) = token(grant(jwt(
        json!({"iss": "app", "sub": "admin@localforce.dev", "aud": "https://login.salesforce.com", "exp": exp}),
    )))
    .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["access_token"].as_str().unwrap().starts_with("00D"));
    for (claims, error) in [
        (
            json!({"iss": "app", "sub": "admin@localforce.dev", "aud": "https://example.com", "exp": exp}),
            "audience is invalid",
        ),
        (
            json!({"iss": "app", "sub": "admin@localforce.dev", "aud": "https://login.salesforce.com", "exp": 1}),
            "expired",
        ),
        (json!({"iss": "app", "sub": "nobody@x.dev", "aud": "https://login.salesforce.com", "exp": exp}), "approved"),
    ] {
        let (s, v) = token(grant(jwt(claims))).await;
        assert_eq!(s, 400);
        assert!(v["error_description"].as_str().unwrap().contains(error), "{v}");
    }
}

#[tokio::test]
async fn soap_calls() {
    use reqwest::Method;
    let org = Org::start().await;
    let soap = |path: &'static str, op: String| {
        let org = &org;
        async move {
            let body = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><soapenv:Header><SessionHeader><sessionId>any</sessionId></SessionHeader></soapenv:Header><soapenv:Body>{op}</soapenv:Body></soapenv:Envelope>"#
            );
            let resp = org.http.post(format!("{}{path}", org.base)).body(body).send().await.unwrap();
            (resp.status().as_u16(), resp.text().await.unwrap())
        }
    };
    let tag = |xml: &str, name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let start = xml.find(&open)? + open.len();
        Some(xml[start..start + xml[start..].find('<')?].to_string())
    };

    // Lead conversion: an account, a contact and an opportunity, and the lead is locked.
    let lead = org
        .create(
            "Lead",
            json!({"FirstName": "Ada", "LastName": "Lovelace", "Company": "Engines", "Email": "ada@example.com", "City": "London", "LeadSource": "Web"}),
        )
        .await;
    let (s, x) = soap(
        "/services/Soap/u/62.0",
        format!("<convertLead><leadConverts><leadId>{lead}</leadId><convertedStatus>Open - Not Contacted</convertedStatus></leadConverts></convertLead>"),
    )
    .await;
    assert_eq!((s, tag(&x, "statusCode").as_deref()), (200, Some("INVALID_STATUS")), "{x}");
    let (s, x) = soap(
        "/services/Soap/u/62.0",
        format!("<convertLead><leadConverts><leadId>{lead}</leadId><convertedStatus>Closed - Converted</convertedStatus></leadConverts></convertLead>"),
    )
    .await;
    assert_eq!((s, tag(&x, "success").as_deref()), (200, Some("true")), "{x}");
    let (account, contact, opp) =
        (tag(&x, "accountId").unwrap(), tag(&x, "contactId").unwrap(), tag(&x, "opportunityId").unwrap());
    let q = org
        .query(&format!(
            "SELECT IsConverted, Status, ConvertedAccount.Name, ConvertedContact.Email, ConvertedContact.MailingCity, ConvertedOpportunity.Name, ConvertedOpportunity.StageName FROM Lead WHERE Id = '{lead}'"
        ))
        .await;
    let l = &q["records"][0];
    assert_eq!(
        (&l["IsConverted"], &l["Status"], &l["ConvertedAccount"]["Name"], &l["ConvertedContact"]["Email"]),
        (&json!(true), &json!("Closed - Converted"), &json!("Engines"), &json!("ada@example.com"))
    );
    assert_eq!(
        (&l["ConvertedContact"]["MailingCity"], &l["ConvertedOpportunity"]["StageName"]),
        (&json!("London"), &json!("Prospecting"))
    );
    let q = org
        .query(&format!("SELECT ContactId, IsPrimary FROM OpportunityContactRole WHERE OpportunityId = '{opp}'"))
        .await;
    assert_eq!(q["records"][0]["ContactId"], json!(contact));
    let (s, v) = org.call(Method::PATCH, &format!("/sobjects/Lead/{lead}"), Some(json!({"Company": "Other"}))).await;
    assert_eq!((s, v[0]["errorCode"].as_str()), (400, Some("CANNOT_UPDATE_CONVERTED_LEAD")), "{v}");
    let (_, x) = soap(
        "/services/Soap/u/62.0",
        format!("<convertLead><leadConverts><leadId>{lead}</leadId><convertedStatus>Closed - Converted</convertedStatus></leadConverts></convertLead>"),
    )
    .await;
    assert_eq!(tag(&x, "statusCode").as_deref(), Some("CANNOT_UPDATE_CONVERTED_LEAD"), "{x}");

    // Into an existing account, without an opportunity.
    let lead2 = org.create("Lead", json!({"LastName": "Babbage", "Company": "Engines"})).await;
    let (_, x) = soap(
        "/services/Soap/u/62.0",
        format!("<convertLead><leadConverts><leadId>{lead2}</leadId><accountId>{account}</accountId><convertedStatus>Closed - Converted</convertedStatus><doNotCreateOpportunity>true</doNotCreateOpportunity></leadConverts></convertLead>"),
    )
    .await;
    assert_eq!((tag(&x, "accountId"), tag(&x, "success").as_deref()), (Some(account.clone()), Some("true")), "{x}");
    assert!(x.contains(r#"<opportunityId xsi:nil="true"/>"#), "{x}");

    // Merging: the others are deleted, pointing at the master, and their children move to it.
    let a = org.create("Account", json!({"Name": "Master"})).await;
    let b = org.create("Account", json!({"Name": "Dupe 1"})).await;
    let c = org.create("Account", json!({"Name": "Dupe 2"})).await;
    let child = org.create("Contact", json!({"LastName": "Moved", "AccountId": b})).await;
    let (_, x) = soap(
        "/services/Soap/u/62.0",
        format!("<merge><request><masterRecord xsi:type=\"sObject\"><type>Account</type><Id>{a}</Id><Phone>555-0199</Phone></masterRecord><recordToMergeIds>{b}</recordToMergeIds><recordToMergeIds>{c}</recordToMergeIds></request></merge>"),
    )
    .await;
    assert_eq!((tag(&x, "id"), tag(&x, "success").as_deref()), (Some(a.clone()), Some("true")), "{x}");
    assert_eq!(x.matches("<mergedRecordIds>").count(), 2, "{x}");
    assert_eq!(tag(&x, "updatedRelatedIds"), Some(child.clone()), "{x}");
    let q = org.query(&format!("SELECT AccountId FROM Contact WHERE Id = '{child}'")).await;
    assert_eq!(q["records"][0]["AccountId"], json!(a));
    let (_, gone) = org
        .call(
            Method::GET,
            &format!(
                "/queryAll?q={}",
                urlencode(&format!("SELECT IsDeleted, MasterRecordId FROM Account WHERE Id = '{b}'"))
            ),
            None,
        )
        .await;
    assert_eq!((&gone["records"][0]["IsDeleted"], &gone["records"][0]["MasterRecordId"]), (&json!(true), &json!(a)));
    let (_, x) = soap(
        "/services/Soap/u/62.0",
        format!("<merge><request><masterRecord xsi:type=\"sObject\"><type>Account</type><Id>{a}</Id></masterRecord><recordToMergeIds>{a}</recordToMergeIds></request></merge>"),
    )
    .await;
    assert_eq!(tag(&x, "statusCode").as_deref(), Some("INVALID_ID_FIELD"), "{x}");

    // Undelete: back from the recycle bin, or an error for a record that isn't in it.
    let (_, x) = soap("/services/Soap/u/62.0", format!("<undelete><ids>{b}</ids><ids>{a}</ids></undelete>")).await;
    assert!(x.contains(&format!("<result><id>{b}</id><success>true</success></result>")), "{x}");
    assert!(x.contains("<statusCode>UNDELETE_FAILED</statusCode>"), "{x}");

    // Helpers, and an unknown call.
    let (_, x) = soap("/services/Soap/u/62.0", "<getUserInfo/>".into()).await;
    assert_eq!(tag(&x, "userName").as_deref(), Some("admin@localforce.dev"), "{x}");
    let (_, x) = soap("/services/Soap/u/62.0", "<describeTabs/>".into()).await;
    assert!(x.contains("<name>standard-Account</name>"), "{x}");
    let (s, x) = soap("/services/Soap/u/62.0", "<emptyRecycleBin/>".into()).await;
    assert_eq!(s, 500, "{x}");

    // The Metadata API reads a picklist, with which lead statuses are converted.
    let (s, x) = soap(
        "/services/Soap/m/62.0",
        "<readMetadata><type>CustomField</type><fullNames>Lead.Status</fullNames></readMetadata>".into(),
    )
    .await;
    assert_eq!(s, 200, "{x}");
    assert!(x.contains("<fullName>Closed - Converted</fullName><converted>true</converted>"), "{x}");

    // Bulk API 1.0 refuses an empty batch.
    let job = org
        .http
        .post(format!("{}/services/async/62.0/job", org.base))
        .header("X-SFDC-Session", "any")
        .header("Content-Type", "application/json")
        .body(r#"{"operation":"update","object":"Account","contentType":"CSV"}"#)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let resp = org
        .http
        .post(format!("{}/services/async/62.0/job/{}/batch", org.base, job["id"].as_str().unwrap()))
        .header("X-SFDC-Session", "any")
        .header("Content-Type", "text/csv")
        .body("")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let text = resp.text().await.unwrap();
    assert!(text.contains("ClientInputError") && text.contains("Unable to find any data to create batch"), "{text}");
}
