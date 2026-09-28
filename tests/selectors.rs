//! Selector Object (spec 5.8.13) and Expression Type Object (5.8.12):
//! JSONPath, XPath and JSON Pointer selection over a runtime-expression
//! context. One node → its value; several → an array; none → null.

use arazzo_jrpc::expr::Scope;
use arazzo_jrpc::model::Selector;
use arazzo_jrpc::select;
use serde_json::{json, Value};

fn scope() -> Scope {
    let mut s = Scope { inputs: json!({"user": {"id": 42, "profile": {"email": "neo@x"}}, "list": [{"sku": "A", "q": 1}, {"sku": "B", "q": 2}]}), ..Default::default() };
    s.set_step_output("fetchXml", "invoiceXml", json!("<Invoice><Header><InvoiceNumber>INV-7</InvoiceNumber></Header><Line>1</Line><Line>2</Line></Invoice>"));
    s
}

fn sel(yaml: &str) -> Value {
    let s: Selector = serde_yaml_ng::from_str(yaml).unwrap();
    select::select(&s, &scope()).unwrap_or_else(|e| panic!("{yaml}: {e}"))
}

#[test]
fn jsonpath_selects_one_many_or_none() {
    assert_eq!(sel("{ context: $inputs.user, selector: $.profile.email, type: jsonpath }"), json!("neo@x"));
    assert_eq!(sel("{ context: $inputs.list, selector: '$[*].sku', type: jsonpath }"), json!(["A", "B"]));
    assert_eq!(sel("{ context: $inputs.list, selector: '$[?@.sku == \"Z\"]', type: jsonpath }"), Value::Null);
}

#[test]
fn jsonpointer_selects() {
    assert_eq!(sel("{ context: $inputs.user, selector: /profile/email, type: jsonpointer }"), json!("neo@x"));
    assert_eq!(
        sel("{ context: $inputs.list, selector: /1/q, type: { type: jsonpointer, version: rfc6901 } }"),
        json!(2)
    );
}

#[test]
#[cfg(feature = "xpath")]
fn xpath_selects_text_and_lists() {
    assert_eq!(
        sel("{ context: $steps.fetchXml.outputs.invoiceXml, selector: /Invoice/Header/InvoiceNumber, type: xpath }"),
        json!("INV-7")
    );
    assert_eq!(
        sel("{ context: $steps.fetchXml.outputs.invoiceXml, selector: /Invoice/Line, type: { type: xpath, version: xpath-30 } }"),
        json!(["1", "2"])
    );
    assert_eq!(
        sel("{ context: $steps.fetchXml.outputs.invoiceXml, selector: 'count(/Invoice/Line)', type: { type: xpath, version: xpath-10 } }"),
        json!(2.0)
    );
}

#[test]
fn goessner_jsonpath_version_is_honoured() {
    assert_eq!(
        sel("{ context: $inputs.list, selector: '$[?(@.q > 1)].sku', type: { type: jsonpath, version: draft-goessner-dispatch-jsonpath-00 } }"),
        json!("B")
    );
}

#[test]
fn selection_errors_name_the_problem() {
    let bad: Selector = serde_yaml_ng::from_str("{ context: $inputs.user, selector: '$[[', type: jsonpath }").unwrap();
    assert!(select::select(&bad, &scope()).unwrap_err().to_string().contains("$[["));
    let no_ctx: Selector = serde_yaml_ng::from_str("{ context: $inputs.nope, selector: $.a, type: jsonpath }").unwrap();
    assert!(select::select(&no_ctx, &scope()).unwrap_err().to_string().contains("nope"));
    let not_xml: Selector = serde_yaml_ng::from_str("{ context: $inputs.user, selector: /a, type: xpath }").unwrap();
    assert!(select::select(&not_xml, &scope()).is_err(), "XPath over a JSON object is an error");
}
