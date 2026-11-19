//! Tabs and theme (`/tabs`, `/theme`, SOAP `describeTabs`): a tab per object a
//! user works with, with the Lightning icon and colour Salesforce gives it.

use serde_json::{Value, json};

use crate::schema::{Catalog, SObject};

/// Standard objects with a tab, their SLDS icon and colour, in the Sales app's order.
const STANDARD: &[(&str, &str, &str)] = &[
    ("Account", "account", "7F8DE1"),
    ("Contact", "contact", "A094ED"),
    ("Lead", "lead", "F88962"),
    ("Opportunity", "opportunity", "FCB95B"),
    ("Case", "case", "F2CF5B"),
    ("Campaign", "campaign", "F49756"),
    ("Contract", "contract", "6EC06E"),
    ("Order", "orders", "769ED9"),
    ("Product2", "product", "B781D3"),
    ("Asset", "asset_object", "317A92"),
    ("Pricebook2", "pricebook", "B781D3"),
    ("Task", "task", "4BC076"),
    ("Event", "event", "EB7092"),
    ("User", "user", "65CAE4"),
];

const CUSTOM: (&str, &str) = ("custom", "8199AF");

fn style(o: &SObject) -> (String, &'static str) {
    match STANDARD.iter().find(|(n, ..)| *n == o.name) {
        Some((_, icon, color)) => (format!("standard/{icon}"), color),
        None => (format!("custom/{}", CUSTOM.0), CUSTOM.1),
    }
}

fn icons(base: &str, o: &SObject) -> Value {
    let (icon, _) = style(o);
    let name = icon.rsplit('/').next().unwrap_or_default().to_string();
    json!([
        { "contentType": "image/svg+xml", "height": 0, "theme": "theme4", "url": format!("{base}/img/icon/t4v35/{icon}.svg"), "width": 0 },
        { "contentType": "image/png", "height": 60, "theme": "theme4", "url": format!("{base}/img/icon/t4v35/{icon}_60.png"), "width": 60 },
        { "contentType": "image/png", "height": 120, "theme": "theme4", "url": format!("{base}/img/icon/t4v35/{icon}_120.png"), "width": 120 },
        { "contentType": "image/png", "height": 32, "theme": "theme3", "url": format!("{base}/img/icon/{name}32.png"), "width": 32 },
        { "contentType": "image/png", "height": 16, "theme": "theme3", "url": format!("{base}/img/icon/{name}16.png"), "width": 16 },
    ])
}

fn colors(o: &SObject) -> Value {
    let (_, color) = style(o);
    json!([{ "color": color, "context": "primary", "theme": "theme4" }])
}

/// The objects that have a tab: the standard ones above that exist, then custom objects.
fn tabbed(cat: &Catalog) -> Vec<&SObject> {
    let standard = STANDARD.iter().filter_map(|(n, ..)| cat.object(n)).filter(|o| !o.custom);
    standard.chain(cat.objects.iter().filter(|o| o.custom)).collect()
}

pub fn tab(base: &str, o: &SObject) -> Value {
    let icons = icons(base, o);
    let icon_url = icons[3]["url"].clone();
    let mini = icons[4]["url"].clone();
    json!({
        "colors": colors(o),
        "custom": o.custom,
        "iconUrl": icon_url,
        "icons": icons,
        "label": o.label_plural,
        "miniIconUrl": mini,
        "name": if o.custom { o.name.clone() } else { format!("standard-{}", o.name) },
        "sobjectName": o.name,
        "url": format!("{base}/{}/o", o.key_prefix),
    })
}

/// `/tabs`: every tab the user can see.
pub fn tabs(base: &str, cat: &Catalog) -> Vec<Value> {
    tabbed(cat).into_iter().map(|o| tab(base, o)).collect()
}

/// `/theme`: the icons and colours of every object.
pub fn theme(base: &str, cat: &Catalog) -> Value {
    let items: Vec<Value> =
        cat.objects.iter().map(|o| json!({ "colors": colors(o), "icons": icons(base, o), "name": o.name })).collect();
    json!({ "themeItems": items })
}
