//! The sales objects' own logic: opportunity products and their price book
//! rules, contact roles, campaign members, contracts, orders and assets, and
//! the summary fields their parents keep (Opportunity.Amount from its
//! products, Order.TotalAmount, a Campaign's member and opportunity counts).
//!
//! `before_save` runs with the values being written, before the row is;
//! `after_save` runs after the row is written, deleted or restored, inside
//! the same savepoint, and keeps parents' summaries up to date.

use std::collections::HashMap;

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension};

use crate::error::{SfError, SfResult};
use crate::schema::{Catalog, Field, SObject};
use crate::store::{Ctx, bool_sql, given, qi, set, sql_text, value_of};

type Values = Vec<(Field, Sql)>;
type Old<'a> = Option<&'a HashMap<String, Sql>>;

fn integrity(field: &str, message: impl Into<String>) -> SfError {
    SfError::new("FIELD_INTEGRITY_EXCEPTION", message).with_fields(vec![field.into()])
}

fn order_locked() -> SfError {
    SfError::new(
        "ENTITY_IS_LOCKED",
        "Unable to modify activated Order: cannot delete Order, or add or remove Order Products",
    )
}

fn num(v: &Sql) -> Option<f64> {
    match v {
        Sql::Integer(n) => Some(*n as f64),
        Sql::Real(n) => Some(*n),
        _ => None,
    }
}

fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

/// A price book entry, as the line item objects need it.
struct Entry {
    id: String,
    book: String,
    product: String,
    price: Option<f64>,
    active: bool,
    name: Option<String>,
    code: Option<String>,
}

fn entry(conn: &Connection, id: &str) -> SfResult<Option<Entry>> {
    Ok(conn
        .query_row(
            "SELECT \"Id\", \"Pricebook2Id\", \"Product2Id\", \"UnitPrice\", \"IsActive\", \"Name\", \"ProductCode\"
             FROM \"PricebookEntry\" WHERE \"Id\" = ?1 AND \"IsDeleted\" = 0",
            [id],
            |r| {
                Ok(Entry {
                    id: r.get(0)?,
                    book: r.get(1)?,
                    product: r.get(2)?,
                    price: r.get(3)?,
                    active: r.get::<_, Option<bool>>(4)?.unwrap_or(false),
                    name: r.get(5)?,
                    code: r.get(6)?,
                })
            },
        )
        .optional()?)
}

/// A field of another record.
fn lookup<T: rusqlite::types::FromSql>(conn: &Connection, object: &str, field: &str, id: &str) -> SfResult<Option<T>> {
    Ok(conn
        .query_row(&format!("SELECT {} FROM {} WHERE \"Id\" = ?1", qi(field), qi(object)), [id], |r| r.get(0))
        .optional()?
        .flatten())
}

fn count(conn: &Connection, object: &str, via: &str, id: &str) -> SfResult<i64> {
    Ok(conn.query_row(
        &format!("SELECT COUNT(*) FROM {} WHERE {} = ?1 AND \"IsDeleted\" = 0", qi(object), qi(via)),
        [id],
        |r| r.get(0),
    )?)
}

/// Checks and derived values for the sales objects, before a save.
/// Read-only fields that follow from others: an opportunity's forecast category
/// and fiscal period, a case's contact details. And a converted lead is read-only.
fn derived_fields(conn: &Connection, ctx: &Ctx, obj: &SObject, values: &mut Values, old: Old) -> SfResult<()> {
    let insert = old.is_none();
    let text = |values: &Values, name: &str| sql_text(&value_of(values, old, name)).unwrap_or_default();
    match obj.name.as_str() {
        "Opportunity" => {
            if insert || given(values, "ForecastCategoryName") {
                let category = match text(values, "ForecastCategoryName").as_str() {
                    "Best Case" => "BestCase",
                    "Commit" => "Forecast",
                    "Pipeline" => "Pipeline",
                    "Closed" => "Closed",
                    _ => "Omitted",
                };
                set(obj, values, "ForecastCategory", Sql::Text(category.into()));
            }
            if insert || given(values, "CloseDate") {
                let close = chrono::NaiveDate::parse_from_str(&text(values, "CloseDate"), "%Y-%m-%d").ok();
                let (year, quarter) = match close {
                    Some(d) => {
                        use chrono::Datelike;
                        (Sql::Integer(d.year() as i64), Sql::Integer(d.month0() as i64 / 3 + 1))
                    }
                    None => (Sql::Null, Sql::Null),
                };
                let fiscal = match (&year, &quarter) {
                    (Sql::Integer(y), Sql::Integer(q)) => Sql::Text(format!("{y} {q}")),
                    _ => Sql::Null,
                };
                set(obj, values, "FiscalYear", year);
                set(obj, values, "FiscalQuarter", quarter);
                set(obj, values, "Fiscal", fiscal);
            }
            let stage_changed = old.is_some_and(|o| {
                given(values, "StageName") && o.get("StageName").and_then(sql_text) != Some(text(values, "StageName"))
            });
            if stage_changed {
                set(obj, values, "LastStageChangeDate", Sql::Text(ctx.now_str()));
            }
            if insert {
                set(obj, values, "HasOpenActivity", bool_sql(false));
                set(obj, values, "HasOverdueTask", bool_sql(false));
            }
        }
        "Case" if insert || given(values, "ContactId") => {
            let contact = text(values, "ContactId");
            let details: (Option<String>, Option<String>, Option<String>, Option<String>) = if contact.is_empty() {
                Default::default()
            } else {
                conn.query_row(
                    "SELECT \"Phone\", \"MobilePhone\", \"Email\", \"Fax\" FROM \"Contact\" WHERE \"Id\" = ?1",
                    [&contact],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?
                .unwrap_or_default()
            };
            let opt = |v: Option<String>| v.map(Sql::Text).unwrap_or(Sql::Null);
            set(obj, values, "ContactPhone", opt(details.0));
            set(obj, values, "ContactMobile", opt(details.1));
            set(obj, values, "ContactEmail", opt(details.2));
            set(obj, values, "ContactFax", opt(details.3));
        }
        "Lead" if old.and_then(|o| o.get("IsConverted")) == Some(&Sql::Integer(1)) => {
            return Err(SfError::new("CANNOT_UPDATE_CONVERTED_LEAD", "cannot reference converted lead"));
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn before_save(
    conn: &Connection,
    _cat: &Catalog,
    ctx: &Ctx,
    obj: &SObject,
    values: &mut Values,
    old: Old,
) -> SfResult<()> {
    let insert = old.is_none();
    let text = |values: &Values, name: &str| sql_text(&value_of(values, old, name)).unwrap_or_default();
    let changed = |values: &Values, name: &str| {
        given(values, name)
            && old.is_some_and(|o| o.get(name).cloned().unwrap_or(Sql::Null) != value_of(values, None, name))
    };
    derived_fields(conn, ctx, obj, values, old)?;
    match obj.name.as_str() {
        "Opportunity" if changed(values, "Pricebook2Id") => {
            if old.and_then(|o| o.get("HasOpportunityLineItem")) == Some(&Sql::Integer(1)) {
                return Err(SfError::new("FIELD_INTEGRITY_EXCEPTION", "cannot change pricebook"));
            }
        }
        "OpportunityLineItem" => line_item(conn, obj, values, old, LineOf::Opportunity)?,
        "OrderItem" => line_item(conn, obj, values, old, LineOf::Order)?,
        "CampaignMember" => {
            if insert {
                let (lead, contact) = (text(values, "LeadId"), text(values, "ContactId"));
                let (member, object) = match (lead.is_empty(), contact.is_empty()) {
                    (_, false) => {
                        // Given both, Salesforce keeps only the contact.
                        set(obj, values, "LeadId", Sql::Null);
                        (contact, "Contact")
                    }
                    (false, true) => (lead, "Lead"),
                    (true, true) => {
                        return Err(SfError::new(
                            "REQUIRED_FIELD_MISSING",
                            format!(
                                "Attempted to add a campaign member where either the member id 'null' or the campaign id '{}' is null.",
                                text(values, "CampaignId")
                            ),
                        ));
                    }
                };
                let campaign = text(values, "CampaignId");
                let field = if object == "Lead" { "LeadId" } else { "ContactId" };
                let dup: bool = conn.query_row(
                    &format!(
                        "SELECT EXISTS (SELECT 1 FROM \"CampaignMember\" WHERE \"CampaignId\" = ?1 AND {} = ?2 AND \"IsDeleted\" = 0)",
                        qi(field)
                    ),
                    [&campaign, &member],
                    |r| r.get(0),
                )?;
                if dup {
                    return Err(SfError::new(
                        "DUPLICATE_VALUE",
                        format!(
                            "Attempted to add an entity '{}' to a campaign '{}' more than once.",
                            &member[..member.len().min(15)],
                            &campaign[..campaign.len().min(15)]
                        ),
                    ));
                }
                let (first, last, email): (Option<String>, Option<String>, Option<String>) = conn
                    .query_row(
                        &format!("SELECT \"FirstName\", \"LastName\", \"Email\" FROM {} WHERE \"Id\" = ?1", qi(object)),
                        [&member],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?
                    .unwrap_or_default();
                let name = [first.as_deref(), last.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
                let opt = |s: Option<String>| s.map(Sql::Text).unwrap_or(Sql::Null);
                set(obj, values, "FirstName", opt(first));
                set(obj, values, "LastName", opt(last));
                set(obj, values, "Name", if name.is_empty() { Sql::Null } else { Sql::Text(name) });
                set(obj, values, "Email", opt(email));
            }
            if insert || given(values, "Status") {
                let responded = text(values, "Status").eq_ignore_ascii_case("Responded");
                set(obj, values, "HasResponded", bool_sql(responded));
                let first = old.and_then(|o| o.get("FirstRespondedDate")).cloned().unwrap_or(Sql::Null);
                if responded && first == Sql::Null {
                    set(obj, values, "FirstRespondedDate", Sql::Text(ctx.now.format("%Y-%m-%d").to_string()));
                }
            }
        }
        "Contract" => {
            let status = text(values, "Status");
            if insert || given(values, "Status") {
                let code = match status.as_str() {
                    "In Approval Process" => "InApproval",
                    "Activated" => "Activated",
                    _ => "Draft",
                };
                if insert && code == "Activated" {
                    return Err(SfError::new(
                        "FAILED_ACTIVATION",
                        "Choose a valid contract status and save your changes. Ask your admin for details.",
                    ));
                }
                let was = old.and_then(|o| o.get("StatusCode")).and_then(sql_text);
                if was.as_deref() == Some("Activated") && code != "Activated" {
                    return Err(integrity("Status", "Status: An activated contract's status can't be changed back."));
                }
                set(obj, values, "StatusCode", Sql::Text(code.into()));
                if code == "Activated" && was.as_deref() != Some("Activated") {
                    set(obj, values, "ActivatedDate", Sql::Text(ctx.now_str()));
                }
            }
            if insert || given(values, "StartDate") || given(values, "ContractTerm") {
                let start = chrono::NaiveDate::parse_from_str(&text(values, "StartDate"), "%Y-%m-%d").ok();
                let term = num(&value_of(values, old, "ContractTerm")).map(|n| n as u32);
                let end = start
                    .zip(term)
                    .and_then(|(s, t)| s.checked_add_months(chrono::Months::new(t)))
                    .and_then(|d| d.pred_opt());
                set(obj, values, "EndDate", end.map(|d| Sql::Text(d.to_string())).unwrap_or(Sql::Null));
            }
        }
        "Order" => {
            let id = old.and_then(|o| o.get("Id")).and_then(sql_text).unwrap_or_default();
            let items = if insert { 0 } else { count(conn, "OrderItem", "OrderId", &id)? };
            if changed(values, "Pricebook2Id") && items > 0 {
                return Err(integrity("Pricebook2Id", "cannot change pricebook on order with order items"));
            }
            if insert || given(values, "Status") {
                let activated = text(values, "Status").eq_ignore_ascii_case("Activated");
                let was = old.and_then(|o| o.get("StatusCode")).and_then(sql_text);
                if activated && insert {
                    return Err(SfError::new(
                        "FAILED_ACTIVATION",
                        "Choose a valid status and save your changes. For a new or cloned order, choose Draft.",
                    ));
                }
                if activated && was.as_deref() != Some("Activated") {
                    if items == 0 {
                        return Err(SfError::new("FAILED_ACTIVATION", "An order must have at least one product."));
                    }
                    set(obj, values, "ActivatedDate", Sql::Text(ctx.now_str()));
                }
                set(obj, values, "StatusCode", Sql::Text(if activated { "Activated" } else { "Draft" }.into()));
            }
        }
        "Asset" if text(values, "AccountId").is_empty() => {
            let contact = text(values, "ContactId");
            let contact_account: Option<String> =
                if contact.is_empty() { None } else { lookup(conn, "Contact", "AccountId", &contact)? };
            if contact_account.is_none() {
                return Err(SfError::new(
                    "FIELD_INTEGRITY_EXCEPTION",
                    "Every asset needs an account, a contact, or both.",
                )
                .with_fields(vec!["AccountId".into(), "ContactId".into()]));
            }
        }
        _ => {}
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum LineOf {
    Opportunity,
    Order,
}

/// An opportunity or order product: its price book entry, product and prices.
fn line_item(conn: &Connection, obj: &SObject, values: &mut Values, old: Old, of: LineOf) -> SfResult<()> {
    let insert = old.is_none();
    let text = |values: &Values, name: &str| sql_text(&value_of(values, old, name)).unwrap_or_default();
    let (parent_obj, parent_field) = match of {
        LineOf::Opportunity => ("Opportunity", "OpportunityId"),
        LineOf::Order => ("Order", "OrderId"),
    };
    let parent = text(values, parent_field);
    if of == LineOf::Order && insert {
        let status: Option<String> = lookup(conn, "Order", "StatusCode", &parent)?;
        if status.as_deref() == Some("Activated") {
            return Err(order_locked());
        }
    }
    if insert {
        let book: Option<String> = lookup(conn, parent_obj, "Pricebook2Id", &parent)?;
        let given_entry = text(values, "PricebookEntryId");
        if given_entry.is_empty() && of == LineOf::Order {
            return Err(SfError::new("REQUIRED_FIELD_MISSING", "Required fields are missing: [PricebookEntryId]")
                .with_fields(vec!["PricebookEntryId".into()]));
        }
        if given_entry.is_empty() {
            return Err(SfError::new(
                "FIELD_INTEGRITY_EXCEPTION",
                "field integrity exception: PricebookEntryId, unknown (versions 3.0 and higher must specify pricebook entry id, others must specify product id)",
            )
            .with_fields(vec!["PricebookEntryId".into(), "unknown".into()]));
        }
        let e = entry(conn, &given_entry)?;
        let Some(e) = e else {
            return Err(SfError::new("INVALID_CROSS_REFERENCE_KEY", "invalid cross reference id")
                .with_fields(vec!["PricebookEntryId".into()]));
        };
        if !e.active {
            return Err(integrity(
                "PricebookEntryId",
                "field integrity exception: PricebookEntryId (pricebook entry is inactive)",
            ));
        }
        match (&book, of) {
            (Some(b), LineOf::Opportunity) if *b != e.book => {
                return Err(integrity(
                    "PricebookEntryId",
                    "field integrity exception: PricebookEntryId (pricebook entry is in a different pricebook than the one assigned to the opportunity)",
                ));
            }
            (Some(b), LineOf::Order) if *b != e.book => {
                return Err(SfError::new(
                    "FIELD_INTEGRITY_EXCEPTION",
                    "Order Products must have a Price Book Entry that belongs to the Price Book related to the parent Order.",
                ));
            }
            (None, LineOf::Order) => {
                return Err(SfError::new("FIELD_INTEGRITY_EXCEPTION", "Price Book Not Set on Order"));
            }
            _ => {}
        }
        set(obj, values, "PricebookEntryId", Sql::Text(e.id.clone()));
        set(obj, values, "Product2Id", Sql::Text(e.product.clone()));
        set(obj, values, "ListPrice", e.price.map(Sql::Real).unwrap_or(Sql::Null));
        if of == LineOf::Opportunity {
            let opp_name: Option<String> = lookup(conn, "Opportunity", "Name", &parent)?;
            let name = [opp_name, e.name].into_iter().flatten().collect::<Vec<_>>().join(" ");
            set(obj, values, "Name", Sql::Text(name));
            set(obj, values, "ProductCode", e.code.map(Sql::Text).unwrap_or(Sql::Null));
        }
    }

    let quantity = num(&value_of(values, old, "Quantity")).unwrap_or(0.0);
    if of == LineOf::Order {
        let unit = num(&value_of(values, old, "UnitPrice")).unwrap_or(0.0);
        set(obj, values, "TotalPrice", Sql::Real(round2(quantity * unit)));
        return Ok(());
    }

    let given_num =
        |values: &Values, name: &str| given(values, name) && !matches!(value_of(values, None, name), Sql::Null);
    let (unit_given, total_given) = (given_num(values, "UnitPrice"), given_num(values, "TotalPrice"));
    if unit_given && total_given {
        return Err(integrity(
            "UnitPrice",
            "field integrity exception: UnitPrice (only one of unit price or total price may be specified)",
        ));
    }
    if insert && !unit_given && !total_given {
        return Err(integrity("TotalPrice", "field integrity exception: TotalPrice (total price must be specified)"));
    }
    if quantity == 0.0 {
        return Err(integrity("Quantity", "field integrity exception: Quantity (quantity must be nonzero)"));
    }
    let (quantity_given, discount_given) = (given(values, "Quantity"), given(values, "Discount"));
    if !insert && quantity_given && discount_given && !unit_given && !total_given {
        return Err(SfError::new(
            "FIELD_INTEGRITY_EXCEPTION",
            "field integrity exception: unknown (specify UnitPrice or TotalPrice when changing both Quantity and Discount)",
        ));
    }
    let discount = num(&value_of(values, old, "Discount")).unwrap_or(0.0);
    let factor = 1.0 - discount / 100.0;
    let unit_for = |total: f64| if factor == 0.0 { 0.0 } else { round2(total / quantity / factor) };
    // Salesforce's rules: a new total sets the unit price; a new quantity on
    // its own keeps the total and adjusts the unit price; otherwise (a new
    // unit price or discount) the unit price is kept and the total follows.
    let (unit, total) = if total_given {
        let total = num(&value_of(values, None, "TotalPrice")).unwrap_or(0.0);
        (unit_for(total), round2(total))
    } else if !insert && quantity_given && !unit_given {
        let total = num(&value_of(values, old, "TotalPrice")).unwrap_or(0.0);
        (unit_for(total), total)
    } else {
        let unit = num(&value_of(values, old, "UnitPrice")).unwrap_or(0.0);
        (unit, round2(quantity * unit * factor))
    };
    set(obj, values, "UnitPrice", Sql::Real(unit));
    set(obj, values, "Subtotal", Sql::Real(round2(quantity * unit)));
    set(obj, values, "TotalPrice", Sql::Real(total));
    Ok(())
}

/// Can a line item be deleted? Not from an activated order.
pub(crate) fn before_delete(conn: &Connection, obj: &SObject, id: &str) -> SfResult<()> {
    match obj.name.as_str() {
        "OrderItem" => {
            let order: Option<String> = lookup(conn, "OrderItem", "OrderId", id)?;
            let status: Option<String> = match order {
                Some(o) => lookup(conn, "Order", "StatusCode", &o)?,
                None => None,
            };
            if status.as_deref() == Some("Activated") {
                return Err(order_locked());
            }
        }
        "Order" if lookup::<String>(conn, "Order", "StatusCode", id)?.as_deref() == Some("Activated") => {
            return Err(order_locked());
        }
        "Contract" if lookup::<String>(conn, "Contract", "StatusCode", id)?.as_deref() == Some("Activated") => {
            return Err(SfError::new("DELETE_FAILED", "You can't delete an activated contract."));
        }
        _ => {}
    }
    Ok(())
}

/// Summary fields a parent keeps over its children: `sets` are the parent's
/// fields and SQL over the child rows (alias `c`; `{p}` is the parent table).
struct Summary {
    child: &'static str,
    via: &'static str,
    parent: &'static str,
    sets: &'static [(&'static str, &'static str)],
}

const SUMMARIES: &[Summary] = &[
    Summary {
        child: "OpportunityLineItem",
        via: "OpportunityId",
        parent: "Opportunity",
        sets: &[
            ("HasOpportunityLineItem", "COUNT(*) > 0"),
            (
                "TotalOpportunityQuantity",
                "CASE WHEN COUNT(*) > 0 THEN SUM(c.\"Quantity\") ELSE {p}.\"TotalOpportunityQuantity\" END",
            ),
            // With products, the amount is their total; without, it's the opportunity's own.
            ("Amount", "CASE WHEN COUNT(*) > 0 THEN round(SUM(c.\"TotalPrice\"), 2) ELSE {p}.\"Amount\" END"),
        ],
    },
    Summary {
        child: "OrderItem",
        via: "OrderId",
        parent: "Order",
        sets: &[("TotalAmount", "round(IFNULL(SUM(c.\"TotalPrice\"), 0), 2)")],
    },
    Summary {
        child: "CampaignMember",
        via: "CampaignId",
        parent: "Campaign",
        sets: &[
            ("NumberOfLeads", "COUNT(*) FILTER (WHERE c.\"LeadId\" IS NOT NULL AND c.\"ContactId\" IS NULL)"),
            ("NumberOfContacts", "COUNT(c.\"ContactId\")"),
            ("NumberOfResponses", "IFNULL(SUM(c.\"HasResponded\"), 0)"),
        ],
    },
    Summary {
        child: "Opportunity",
        via: "CampaignId",
        parent: "Campaign",
        sets: &[
            ("NumberOfOpportunities", "COUNT(*)"),
            ("NumberOfWonOpportunities", "IFNULL(SUM(c.\"IsWon\"), 0)"),
            ("AmountAllOpportunities", "IFNULL(SUM(c.\"Amount\"), 0)"),
            ("AmountWonOpportunities", "IFNULL(SUM(CASE WHEN c.\"IsWon\" = 1 THEN c.\"Amount\" END), 0)"),
        ],
    },
];

fn recalc(conn: &Connection, s: &Summary, parent_id: &str) -> SfResult<()> {
    let p = qi(s.parent);
    let sets: Vec<String> = s
        .sets
        .iter()
        .map(|(field, expr)| {
            format!(
                "{} = (SELECT {} FROM {} c WHERE c.{} = {p}.\"Id\" AND c.\"IsDeleted\" = 0)",
                qi(field),
                expr.replace("{p}", &p),
                qi(s.child),
                qi(s.via)
            )
        })
        .collect();
    conn.execute(&format!("UPDATE {p} SET {} WHERE \"Id\" = ?1", sets.join(", ")), [parent_id])?;
    Ok(())
}

/// After a record is written, deleted or restored: its own summaries and
/// derived values, then its parents' summaries (and theirs, and so on).
pub(crate) fn after_save(
    conn: &Connection,
    cat: &Catalog,
    ctx: &Ctx,
    obj: &SObject,
    id: &str,
    old: Old,
    touch: bool,
) -> SfResult<()> {
    for s in SUMMARIES.iter().filter(|s| s.parent == obj.name) {
        recalc(conn, s, id)?;
    }
    match obj.name.as_str() {
        "Opportunity" => {
            conn.execute(
                "UPDATE \"Opportunity\" SET \"ExpectedRevenue\" = round(\"Amount\" * \"Probability\" / 100.0, 2) WHERE \"Id\" = ?1",
                [id],
            )?;
        }
        // The first product sets the opportunity's price book.
        "OpportunityLineItem" if old.is_none() => {
            conn.execute(
                "UPDATE \"Opportunity\" SET \"Pricebook2Id\" =
                   (SELECT e.\"Pricebook2Id\" FROM \"OpportunityLineItem\" i JOIN \"PricebookEntry\" e ON e.\"Id\" = i.\"PricebookEntryId\" WHERE i.\"Id\" = ?1)
                 WHERE \"Id\" = (SELECT \"OpportunityId\" FROM \"OpportunityLineItem\" WHERE \"Id\" = ?1) AND \"Pricebook2Id\" IS NULL",
                [id],
            )?;
        }
        // One primary contact role per opportunity.
        "OpportunityContactRole" => {
            conn.execute(
                "UPDATE \"OpportunityContactRole\" SET \"IsPrimary\" = 0
                 WHERE \"OpportunityId\" = (SELECT \"OpportunityId\" FROM \"OpportunityContactRole\" WHERE \"Id\" = ?1 AND \"IsPrimary\" = 1 AND \"IsDeleted\" = 0)
                   AND \"Id\" <> ?1",
                [id],
            )?;
        }
        _ => {}
    }
    if touch {
        conn.execute(
            &format!(
                "UPDATE {} SET \"SystemModstamp\" = ?2, \"LastModifiedDate\" = ?2, \"LastModifiedById\" = ?3 WHERE \"Id\" = ?1",
                qi(&obj.name)
            ),
            [id, &ctx.now_str(), &ctx.user_id],
        )?;
    }
    for s in SUMMARIES.iter().filter(|s| s.child == obj.name) {
        let now: Option<String> = lookup(conn, s.child, s.via, id)?;
        let before = old.and_then(|o| o.get(s.via)).and_then(sql_text);
        let mut parents: Vec<String> = now.into_iter().chain(before).collect();
        parents.dedup();
        let parent_obj = cat.object(s.parent).expect("summary parents are standard objects");
        for p in parents {
            // A campaign's counts don't change its last-modified date; an opportunity's amount does.
            after_save(conn, cat, ctx, parent_obj, &p, None, s.parent != "Campaign")?;
        }
    }
    Ok(())
}
