//! Partner SOAP API calls besides `login` and `logout`: the ones REST has no
//! equivalent for (`convertLead`, `merge`, `undelete`, `describeTabs`) and a few
//! session helpers (`getUserInfo`, `getServerTimestamp`).

use axum::http::StatusCode;
use roxmltree::{Document, Node};
use rusqlite::Connection;

use serde_json::{Map, Value, json};

use crate::error::{SfError, SfResult};
use crate::schema::{Catalog, FieldType, SObject};
use crate::state::AppState;
use crate::store::{self, Ctx};

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A SOAP fault, as `(status, body)`.
pub fn fault(code: &str, message: &str) -> (StatusCode, String) {
    let m = esc(message);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!(
            r#"<soapenv:Fault><faultcode>sf:{code}</faultcode><faultstring>{code}: {m}</faultstring><detail><sf:UnexpectedErrorFault xmlns:sf="urn:fault.partner.soap.sforce.com" xsi:type="sf:UnexpectedErrorFault"><sf:exceptionCode>{code}</sf:exceptionCode><sf:exceptionMessage>{m}</sf:exceptionMessage></sf:UnexpectedErrorFault></detail></soapenv:Fault>"#
        ),
    )
}

/// The operation element inside `<Body>`.
pub fn operation<'a, 'i>(doc: &'a Document<'i>) -> Option<Node<'a, 'i>> {
    doc.descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "Body")
        .and_then(|b| b.children().find(|c| c.is_element()))
}

fn child_text(n: Node, name: &str) -> Option<String> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
        .map(|c| c.text().unwrap_or_default().trim().to_string())
}

fn children_text(n: Node, name: &str) -> Vec<String> {
    n.children()
        .filter(|c| c.is_element() && c.tag_name().name() == name)
        .map(|c| c.text().unwrap_or_default().trim().to_string())
        .collect()
}

fn flag(n: Node, name: &str) -> bool {
    child_text(n, name).is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

fn errors_xml(e: &SfError) -> String {
    let fields: String = e.fields.iter().map(|f| format!("<fields>{}</fields>", esc(f))).collect();
    format!("<errors>{fields}<message>{}</message><statusCode>{}</statusCode></errors>", esc(&e.message), e.code)
}

fn opt(name: &str, v: Option<&str>) -> String {
    match v {
        Some(v) => format!("<{name}>{}</{name}>", esc(v)),
        None => format!(r#"<{name} xsi:nil="true"/>"#),
    }
}

/// Run a non-login call. The caller has checked there's a body to parse.
pub fn call(st: &AppState, base: &str, body: &str) -> (StatusCode, String) {
    let Ok(doc) = Document::parse(body) else {
        return fault("INVALID_XML", "The request isn't valid XML");
    };
    let Some(op) = operation(&doc) else {
        return fault("INVALID_OPERATION", "No operation found in the SOAP body");
    };
    let session = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "sessionId")
        .and_then(|n| n.text())
        .unwrap_or_default()
        .trim()
        .to_string();
    let Some(user) = st.user_for_token(&session) else {
        return fault("INVALID_SESSION_ID", "Invalid Session ID found in SessionHeader: Illegal Session");
    };
    st.count_call();
    let name = op.tag_name().name().to_string();
    let ctx = st.ctx(&user, "62.0");
    let cat = st.cat();
    let conn = st.db.lock().unwrap();
    let result = (|| -> SfResult<String> {
        conn.execute_batch("SAVEPOINT lf_soap")?;
        let out = match name.as_str() {
            "getServerTimestamp" => Ok(format!("<result><timestamp>{}</timestamp></result>", ctx.now_str())),
            "getUserInfo" => user_info(&conn, st, &ctx),
            "describeTabs" => Ok(describe_tabs(base, &cat)),
            "undelete" => Ok(undelete(&conn, &cat, &ctx, op)),
            "convertLead" => each(&conn, op, &["leadConverts"], |n| convert_lead(&conn, &cat, &ctx, n)),
            // The WSDL calls a merge `request`; jsforce sends `mergeRequests`, which Salesforce accepts too.
            "merge" => each(&conn, op, &["request", "mergeRequests"], |n| merge(&conn, &cat, &ctx, n)),
            other => {
                Err(SfError::new("INVALID_OPERATION", format!("localforce doesn't emulate the SOAP call {other} yet")))
            }
        };
        match &out {
            Ok(_) => conn.execute_batch("RELEASE lf_soap")?,
            Err(_) => conn.execute_batch("ROLLBACK TO lf_soap; RELEASE lf_soap")?,
        }
        out
    })();
    match result {
        Ok(r) => (StatusCode::OK, format!("<{name}Response>{r}</{name}Response>")),
        Err(e) => fault(e.code, &e.message),
    }
}

fn user_info(conn: &Connection, st: &AppState, ctx: &Ctx) -> SfResult<String> {
    let (username, email, name): (String, String, Option<String>) = conn.query_row(
        "SELECT \"Username\", \"Email\", \"Name\" FROM \"User\" WHERE \"Id\" = ?1",
        [&ctx.user_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(format!(
        "<result><accessibilityMode>false</accessibilityMode><chatterExternal>false</chatterExternal><currencySymbol>$</currencySymbol><orgAttachmentFileSizeLimit>5242880</orgAttachmentFileSizeLimit><orgDefaultCurrencyIsoCode>USD</orgDefaultCurrencyIsoCode><orgDefaultCurrencyLocale>en_US</orgDefaultCurrencyLocale><orgDisallowHtmlAttachments>false</orgDisallowHtmlAttachments><orgHasPersonAccounts>false</orgHasPersonAccounts><organizationId>{org}</organizationId><organizationMultiCurrency>false</organizationMultiCurrency><organizationName>localforce</organizationName><profileId>00eLf0000000001AAA</profileId><roleId xsi:nil=\"true\"/><sessionSecondsValid>7200</sessionSecondsValid><userDefaultCurrencyIsoCode xsi:nil=\"true\"/><userEmail>{email}</userEmail><userFullName>{name}</userFullName><userId>{id}</userId><userLanguage>en_US</userLanguage><userLocale>en_US</userLocale><userName>{username}</userName><userTimeZone>GMT</userTimeZone><userType>Standard</userType><userUiSkin>Theme3</userUiSkin></result>",
        org = st.org_id,
        email = esc(&email),
        name = esc(name.as_deref().unwrap_or_default()),
        id = ctx.user_id,
        username = esc(&username),
    ))
}

fn describe_tabs(base: &str, cat: &Catalog) -> String {
    let tabs: String = crate::tabs::tabs(base, cat)
        .iter()
        .map(|t| {
            let colors: String = t["colors"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| {
                    format!(
                        "<colors><color>{}</color><context>{}</context><theme>{}</theme></colors>",
                        c["color"].as_str().unwrap_or_default(),
                        c["context"].as_str().unwrap_or_default(),
                        c["theme"].as_str().unwrap_or_default()
                    )
                })
                .collect();
            let icons: String = t["icons"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| {
                    format!(
                        "<icons><contentType>{}</contentType><height>{}</height><theme>{}</theme><url>{}</url><width>{}</width></icons>",
                        i["contentType"].as_str().unwrap_or_default(),
                        i["height"],
                        i["theme"].as_str().unwrap_or_default(),
                        esc(i["url"].as_str().unwrap_or_default()),
                        i["width"]
                    )
                })
                .collect();
            let s = |k: &str| esc(t[k].as_str().unwrap_or_default());
            format!(
                "<tabs>{colors}<custom>{}</custom><iconUrl>{}</iconUrl>{icons}<label>{}</label><miniIconUrl>{}</miniIconUrl><name>{}</name><sobjectName>{}</sobjectName><url>{}</url></tabs>",
                t["custom"],
                s("iconUrl"),
                s("label"),
                s("miniIconUrl"),
                s("name"),
                s("sobjectName"),
                s("url")
            )
        })
        .collect();
    format!(
        "<result><description>Manage customer service with accounts, contacts, cases, and more</description><label>Sales</label><logoUrl>{base}/img/seasonLogos/AppLauncher/sales.png</logoUrl><namespace xsi:nil=\"true\"/><selected>true</selected><tabSetId>{}</tabSetId>{tabs}</result>",
        crate::layouts::stable_id("02u", "Sales")
    )
}

fn undelete(conn: &Connection, cat: &Catalog, ctx: &Ctx, op: Node) -> String {
    children_text(op, "ids")
        .iter()
        .map(|id| {
            let r = crate::ids::to18(id)
                .ok_or_else(|| SfError::new("MALFORMED_ID", format!("malformed id {id}")))
                .and_then(|id18| {
                    let o = cat
                        .object_for_id(&id18)
                        .ok_or_else(|| SfError::new("INVALID_ID_FIELD", "invalid record id"))?;
                    store::undelete(conn, cat, ctx, o, &id18)?;
                    Ok(id18)
                });
            match r {
                Ok(id) => format!("<result><id>{id}</id><success>true</success></result>"),
                Err(e) => format!("<result>{}{}<success>false</success></result>", errors_xml(&e), opt("id", None)),
            }
        })
        .collect()
}

/// Run `f` on every `<name>` element of the call (any of `names`), each in its own savepoint:
/// one failing item doesn't undo the others.
fn each(conn: &Connection, op: Node, names: &[&str], mut f: impl FnMut(Node) -> SfResult<String>) -> SfResult<String> {
    let mut out = String::new();
    for (i, n) in op.children().filter(|c| c.is_element() && names.contains(&c.tag_name().name())).enumerate() {
        conn.execute_batch(&format!("SAVEPOINT lf_item_{i}"))?;
        match f(n) {
            Ok(r) => {
                conn.execute_batch(&format!("RELEASE lf_item_{i}"))?;
                out += &r;
            }
            Err(e) => {
                conn.execute_batch(&format!("ROLLBACK TO lf_item_{i}; RELEASE lf_item_{i}"))?;
                out += &format!("<result>{}<success>false</success></result>", errors_xml(&e));
            }
        }
    }
    Ok(out)
}

fn id_of(cat: &Catalog, id: &str, object: &str) -> SfResult<String> {
    let id18 = crate::ids::to18(id).ok_or_else(|| SfError::new("MALFORMED_ID", format!("malformed id {id}")))?;
    match cat.object_for_id(&id18) {
        Some(o) if o.name == object => Ok(id18),
        _ => Err(SfError::new("INVALID_CROSS_REFERENCE_KEY", "invalid cross reference id")),
    }
}

/// The lead fields that conversion copies, and where they go on the account and contact.
const TO_ACCOUNT: &[(&str, &str)] = &[
    ("Phone", "Phone"),
    ("Fax", "Fax"),
    ("Website", "Website"),
    ("Industry", "Industry"),
    ("AnnualRevenue", "AnnualRevenue"),
    ("NumberOfEmployees", "NumberOfEmployees"),
    ("Rating", "Rating"),
    ("Street", "BillingStreet"),
    ("City", "BillingCity"),
    ("State", "BillingState"),
    ("PostalCode", "BillingPostalCode"),
    ("Country", "BillingCountry"),
    ("LeadSource", "AccountSource"),
];
const TO_CONTACT: &[(&str, &str)] = &[
    ("Salutation", "Salutation"),
    ("FirstName", "FirstName"),
    ("LastName", "LastName"),
    ("Title", "Title"),
    ("Email", "Email"),
    ("Phone", "Phone"),
    ("MobilePhone", "MobilePhone"),
    ("Fax", "Fax"),
    ("Street", "MailingStreet"),
    ("City", "MailingCity"),
    ("State", "MailingState"),
    ("PostalCode", "MailingPostalCode"),
    ("Country", "MailingCountry"),
    ("LeadSource", "LeadSource"),
    ("Description", "Description"),
    ("HasOptedOutOfEmail", "HasOptedOutOfEmail"),
    ("HasOptedOutOfFax", "HasOptedOutOfFax"),
    ("DoNotCall", "DoNotCall"),
];

/// The lead's values for `map`, leaving out blanks.
fn mapped(lead: &Value, map: &[(&str, &str)]) -> Map<String, Value> {
    map.iter()
        .filter_map(|(from, to)| match &lead[*from] {
            Value::Null => None,
            v => Some((to.to_string(), v.clone())),
        })
        .collect()
}

/// Copy `values` into an existing record's blank fields, as conversion does.
fn fill_blanks(
    conn: &Connection,
    cat: &Catalog,
    ctx: &Ctx,
    o: &SObject,
    id: &str,
    values: Map<String, Value>,
) -> SfResult<()> {
    let current = store::get(conn, cat, ctx, o, id, None)?;
    let blanks: Map<String, Value> = values.into_iter().filter(|(k, _)| current[k.as_str()].is_null()).collect();
    if !blanks.is_empty() {
        store::update(conn, cat, ctx, o, id, &blanks)?;
    }
    Ok(())
}

/// The last day of the current calendar quarter: a converted opportunity's close date.
fn end_of_quarter(ctx: &Ctx) -> String {
    use chrono::Datelike;
    let today = ctx.now.date_naive();
    let first_month_next = (today.month0() / 3 + 1) * 3 + 1;
    let next = if first_month_next > 12 {
        chrono::NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)
    } else {
        chrono::NaiveDate::from_ymd_opt(today.year(), first_month_next, 1)
    };
    next.and_then(|d| d.pred_opt()).map(|d| d.to_string()).unwrap_or_default()
}

/// The statuses that mean a lead is converted.
const CONVERTED_STATUSES: &[&str] = &["Closed - Converted"];

fn convert_lead(conn: &Connection, cat: &Catalog, ctx: &Ctx, n: Node) -> SfResult<String> {
    let lead_o = cat.object("Lead").expect("Lead is a standard object");
    let lead_id = id_of(cat, &child_text(n, "leadId").unwrap_or_default(), "Lead")?;
    let lead = store::get(conn, cat, ctx, lead_o, &lead_id, None)?;
    if lead["IsConverted"] == json!(true) {
        let contact: Option<String> = conn
            .query_row(
                "SELECT c.\"Name\" FROM \"Lead\" l JOIN \"Contact\" c ON c.\"Id\" = l.\"ConvertedContactId\" WHERE l.\"Id\" = ?1",
                [&lead_id],
                |r| r.get(0),
            )
            .ok();
        let date = lead["ConvertedDate"].as_str().and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        return Err(SfError::new(
            "CANNOT_UPDATE_CONVERTED_LEAD",
            format!(
                "This lead was already converted to the contact {} on {}.",
                contact.unwrap_or_default(),
                date.map(|d| d.format("%-m/%-d/%Y").to_string()).unwrap_or_default()
            ),
        ));
    }
    let status = child_text(n, "convertedStatus").unwrap_or_default();
    if !CONVERTED_STATUSES.iter().any(|s| s.eq_ignore_ascii_case(&status)) {
        return Err(SfError::new("INVALID_STATUS", format!("invalid convertedStatus: {status}")));
    }
    let owner = match child_text(n, "ownerId").filter(|o| !o.is_empty()) {
        Some(o) => id_of(cat, &o, "User")?,
        None => lead["OwnerId"].as_str().unwrap_or(&ctx.user_id).to_string(),
    };
    let given = |name: &str| child_text(n, name).filter(|v| !v.is_empty());

    // The account: the one given (filling its blank fields), or a new one named after the company.
    let account_o = cat.object("Account").expect("Account is a standard object");
    let mut account_values = mapped(&lead, TO_ACCOUNT);
    let account_id = match given("accountId") {
        Some(a) => {
            let a = id_of(cat, &a, "Account")?;
            fill_blanks(conn, cat, ctx, account_o, &a, account_values)?;
            a
        }
        None => {
            let company = lead["Company"].as_str().filter(|c| !c.is_empty());
            let name = company.or(lead["Name"].as_str()).unwrap_or_default();
            account_values.insert("Name".into(), json!(name));
            account_values.insert("OwnerId".into(), json!(owner));
            store::insert(conn, cat, ctx, account_o, &account_values)?
        }
    };

    // The contact: the one given (which must belong to the account), or a new one.
    let contact_o = cat.object("Contact").expect("Contact is a standard object");
    let mut contact_values = mapped(&lead, TO_CONTACT);
    let contact_id = match given("contactId") {
        Some(c) => {
            let c = id_of(cat, &c, "Contact")?;
            let parent: Option<String> =
                conn.query_row("SELECT \"AccountId\" FROM \"Contact\" WHERE \"Id\" = ?1", [&c], |r| r.get(0))?;
            if given("accountId").is_none() || parent.as_deref() != Some(account_id.as_str()) {
                return Err(SfError::new(
                    "FIELD_INTEGRITY_EXCEPTION",
                    "The contact must belong to the account given in accountId.",
                ));
            }
            let source = contact_values.remove("LeadSource");
            fill_blanks(conn, cat, ctx, contact_o, &c, contact_values)?;
            if let Some(source) = source.filter(|_| flag(n, "overwriteLeadSource")) {
                let mut m = Map::new();
                m.insert("LeadSource".into(), source);
                store::update(conn, cat, ctx, contact_o, &c, &m)?;
            }
            c
        }
        None => {
            contact_values.insert("AccountId".into(), json!(account_id));
            contact_values.insert("OwnerId".into(), json!(owner));
            store::insert(conn, cat, ctx, contact_o, &contact_values)?
        }
    };

    // The opportunity, unless told not to: named after the company, closing at the end of the quarter.
    let opportunity_id = if flag(n, "doNotCreateOpportunity") {
        None
    } else {
        let opp_o = cat.object("Opportunity").expect("Opportunity is a standard object");
        let name = given("opportunityName")
            .or_else(|| lead["Company"].as_str().filter(|c| !c.is_empty()).map(String::from))
            .or_else(|| lead["Name"].as_str().map(String::from))
            .unwrap_or_default();
        let mut opp = Map::new();
        opp.insert("Name".into(), json!(name.chars().take(80).collect::<String>()));
        opp.insert("AccountId".into(), json!(account_id));
        opp.insert("OwnerId".into(), json!(owner));
        opp.insert("StageName".into(), json!(crate::schema::OPPORTUNITY_STAGES[0].name));
        opp.insert("CloseDate".into(), json!(end_of_quarter(ctx)));
        if !lead["LeadSource"].is_null() {
            opp.insert("LeadSource".into(), lead["LeadSource"].clone());
        }
        let id = store::insert(conn, cat, ctx, opp_o, &opp)?;
        if let Some(role_o) = cat.object("OpportunityContactRole") {
            let mut role = Map::new();
            role.insert("OpportunityId".into(), json!(id));
            role.insert("ContactId".into(), json!(contact_id));
            role.insert("IsPrimary".into(), json!(true));
            store::insert(conn, cat, ctx, role_o, &role)?;
        }
        Some(id)
    };

    // The lead becomes read-only; its campaign memberships now count the contact too.
    conn.execute(
        "UPDATE \"Lead\" SET \"IsConverted\" = 1, \"ConvertedDate\" = ?2, \"ConvertedAccountId\" = ?3,
           \"ConvertedContactId\" = ?4, \"ConvertedOpportunityId\" = ?5, \"Status\" = ?6,
           \"LastModifiedDate\" = ?7, \"SystemModstamp\" = ?7, \"LastModifiedById\" = ?8
         WHERE \"Id\" = ?1",
        rusqlite::params![
            lead_id,
            ctx.now.date_naive().to_string(),
            account_id,
            contact_id,
            opportunity_id,
            status,
            ctx.now_str(),
            ctx.user_id
        ],
    )?;
    if cat.object("CampaignMember").is_some() {
        conn.execute(
            "UPDATE \"CampaignMember\" SET \"ContactId\" = ?2 WHERE \"LeadId\" = ?1 AND \"ContactId\" IS NULL",
            [&lead_id, &contact_id],
        )?;
    }
    Ok(format!(
        "<result><accountId>{account_id}</accountId><contactId>{contact_id}</contactId><leadId>{lead_id}</leadId>{}<success>true</success></result>",
        opt("opportunityId", opportunity_id.as_deref())
    ))
}

/// An `sObject` element's fields as JSON, typed by the object's describe.
fn sobject_fields(o: &SObject, n: Node) -> Map<String, Value> {
    let mut out = Map::new();
    for c in n.children().filter(|c| c.is_element()) {
        let name = c.tag_name().name();
        if matches!(name, "type" | "Id" | "fieldsToNull") {
            if name == "fieldsToNull" {
                out.insert(c.text().unwrap_or_default().trim().to_string(), Value::Null);
            }
            continue;
        }
        let nil = c.attributes().any(|a| a.name() == "nil" && a.value() == "true");
        let text = c.text().unwrap_or_default().to_string();
        let v = match o.field(name).map(|f| f.ty) {
            _ if nil => Value::Null,
            Some(FieldType::Boolean) => json!(text.eq_ignore_ascii_case("true")),
            Some(FieldType::Int | FieldType::Double | FieldType::Currency | FieldType::Percent) => {
                text.parse::<f64>().map(|x| json!(x)).unwrap_or(json!(text))
            }
            _ => json!(text),
        };
        out.insert(name.to_string(), v);
    }
    out
}

/// Objects that can be merged.
const MERGEABLE: &[&str] = &["Account", "Contact", "Lead"];

fn merge(conn: &Connection, cat: &Catalog, ctx: &Ctx, n: Node) -> SfResult<String> {
    let master = n
        .children()
        .find(|c| c.is_element() && c.tag_name().name() == "masterRecord")
        .ok_or_else(|| SfError::new("INVALID_ARGUMENT_TYPE", "A merge request needs a masterRecord"))?;
    let ty = child_text(master, "type").unwrap_or_default();
    let o = cat
        .object(&ty)
        .filter(|o| MERGEABLE.contains(&o.name.as_str()))
        .ok_or_else(|| SfError::new("INVALID_TYPE", format!("Cannot merge records of type {ty}")))?;
    let master_id = id_of(cat, &child_text(master, "Id").unwrap_or_default(), &o.name)?;
    let others = children_text(n, "recordToMergeIds");
    if others.is_empty() || others.len() > 2 {
        return Err(SfError::new(
            "INVALID_OPERATION",
            "A merge request takes one or two records to merge into the master record.",
        ));
    }
    let mut merged = vec![];
    for id in &others {
        let id = id_of(cat, id, &o.name)?;
        if id == master_id {
            return Err(SfError::new("INVALID_ID_FIELD", "The master record id appears in the merge id list:"));
        }
        merged.push(id);
    }
    let fields = sobject_fields(o, master);
    if !fields.is_empty() {
        store::update(conn, cat, ctx, o, &master_id, &fields)?;
    }
    // Move the merged records' related records to the master, then delete them.
    let mut related = vec![];
    for id in &merged {
        for rel in cat.children_of(&o.name) {
            let child = store::qi(&rel.child_object);
            let ids: Vec<String> = conn
                .prepare(&format!(
                    "SELECT \"Id\" FROM {child} WHERE {} = ?1 AND \"IsDeleted\" = 0",
                    store::qi(&rel.field)
                ))?
                .query_map([id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            conn.execute(
                &format!("UPDATE {child} SET {} = ?2 WHERE {} = ?1", store::qi(&rel.field), store::qi(&rel.field)),
                [id, &master_id],
            )?;
            related.extend(ids);
        }
        conn.execute(
            &format!("UPDATE {} SET \"MasterRecordId\" = ?2 WHERE \"Id\" = ?1", store::qi(&o.name)),
            [id, &master_id],
        )?;
        store::delete(conn, cat, ctx, o, id)?;
    }
    let merged_xml: String = merged.iter().map(|m| format!("<mergedRecordIds>{m}</mergedRecordIds>")).collect();
    let related_xml: String = related.iter().map(|r| format!("<updatedRelatedIds>{r}</updatedRelatedIds>")).collect();
    Ok(format!("<result><id>{master_id}</id>{merged_xml}<success>true</success>{related_xml}</result>"))
}

/// The Metadata API (`/services/Soap/m/`): only `readMetadata` of `CustomField`,
/// which clients use to read picklist values (such as which lead statuses are converted).
pub fn metadata_call(st: &AppState, body: &str) -> (StatusCode, String) {
    let Ok(doc) = Document::parse(body) else {
        return fault("INVALID_XML", "The request isn't valid XML");
    };
    let Some(op) = operation(&doc) else {
        return fault("INVALID_OPERATION", "No operation found in the SOAP body");
    };
    let session = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "sessionId")
        .and_then(|n| n.text())
        .unwrap_or_default()
        .trim()
        .to_string();
    if st.user_for_token(&session).is_none() {
        return fault("INVALID_SESSION_ID", "Invalid Session ID found in SessionHeader: Illegal Session");
    }
    let name = op.tag_name().name();
    let ty = child_text(op, "type").unwrap_or_default();
    if name != "readMetadata" || ty != "CustomField" {
        return fault(
            "INVALID_OPERATION",
            &format!("localforce's Metadata API only reads CustomField, not {name} {ty}"),
        );
    }
    let cat = st.cat();
    let records: String = children_text(op, "fullNames")
        .iter()
        .filter_map(|full| {
            let (object, field) = full.split_once('.')?;
            let f = cat.object(object)?.field(field)?;
            let picklist: String = if f.picklist.is_empty() {
                String::new()
            } else {
                let values: String = f
                    .picklist
                    .iter()
                    .map(|v| {
                        let default = f.default.as_ref().and_then(|d| d.as_str()) == Some(v.as_str());
                        let converted = if object.eq_ignore_ascii_case("Lead") && f.name == "Status" {
                            format!("<converted>{}</converted>", CONVERTED_STATUSES.contains(&v.as_str()))
                        } else {
                            String::new()
                        };
                        format!("<picklistValues><fullName>{}</fullName>{converted}<default>{default}</default></picklistValues>", esc(v))
                    })
                    .collect();
                format!("<picklist>{values}<sorted>false</sorted></picklist>")
            };
            let ty = match f.ty {
                FieldType::Picklist => "Picklist",
                FieldType::Multipicklist => "MultiselectPicklist",
                FieldType::Boolean => "Checkbox",
                FieldType::Reference => "Lookup",
                FieldType::Textarea => "TextArea",
                FieldType::Date => "Date",
                FieldType::Datetime => "DateTime",
                FieldType::Currency => "Currency",
                FieldType::Percent => "Percent",
                FieldType::Int | FieldType::Double => "Number",
                FieldType::Email => "Email",
                FieldType::Phone => "Phone",
                FieldType::Url => "Url",
                _ => "Text",
            };
            Some(format!(
                "<records xsi:type=\"CustomField\"><fullName>{}.{}</fullName><label>{}</label>{picklist}<type>{ty}</type></records>",
                esc(object),
                esc(&f.name),
                esc(&f.label)
            ))
        })
        .collect();
    (StatusCode::OK, format!("<readMetadataResponse><result>{records}</result></readMetadataResponse>"))
}
