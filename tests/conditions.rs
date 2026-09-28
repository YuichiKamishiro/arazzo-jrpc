//! Criterion Object (spec 5.8.11): simple conditions with every operator and
//! the 5.8.11.4.1 conversion rules, regex, JSONPath (RFC 9535 and the
//! Goessner draft), XPath (3.1 default, 3.0/2.0/1.0 by Expression Type), and
//! "an evaluation error is a fail".

use arazzo_jrpc::condition;
use arazzo_jrpc::expr::Scope;
use arazzo_jrpc::model::Criterion;
use arazzo_jrpc::transport::Exchange;
use serde_json::{json, Value};

fn scope_with_body(body: Value) -> Scope {
    let mut s = Scope { inputs: json!({"threshold": 3, "statusPattern": "ok|done", "expected": "active", "name": "Neo"}), ..Default::default() };
    s.set_step_output("getPricing", "minPrice", json!(10));
    s.exchange = Some(Exchange {
        status: 200,
        response_headers: vec![("X-Count".into(), "5".into())],
        response_body: body,
        ..Default::default()
    });
    s
}

fn scope() -> Scope {
    scope_with_body(json!({
        "count": 4, "data": {"id": 1}, "empty": null, "ok": true, "items": ["a", "b"],
        "status": "OK", "price": "19.5", "nested": {"flag": false}
    }))
}

fn crit(yaml: &str) -> Criterion {
    serde_yaml_ng::from_str(yaml).unwrap_or_else(|e| panic!("{yaml}: {e}"))
}

fn simple(c: &str) -> bool {
    condition::passes(&Criterion::simple(c), &scope())
}

#[test]
fn comparison_operators() {
    assert!(simple("$statusCode == 200"));
    assert!(simple("$statusCode != 404"));
    assert!(simple("$response.body.count > $inputs.threshold"));
    assert!(simple("$response.body.count >= 4"));
    assert!(simple("$response.body.count < 5"));
    assert!(simple("$response.body.count <= 4"));
    assert!(!simple("$response.body.count < 4"));
}

#[test]
fn logical_operators_precedence_and_grouping() {
    assert!(simple("$statusCode == 200 && $response.body.data != null"));
    assert!(simple("$statusCode == 500 || $response.body.ok == true"));
    assert!(!simple("!($statusCode == 200)"));
    assert!(simple("!$response.body.nested.flag"));
    // && binds tighter than ||: true || (false && false)
    assert!(simple("$statusCode == 200 || $statusCode == 1 && $statusCode == 2"));
    assert!(!simple("($statusCode == 200 || $statusCode == 1) && $statusCode == 2"));
}

#[test]
fn index_and_property_dereference() {
    assert!(simple("$response.body.items[1] == 'b'"));
    assert!(simple("$response.body.data.id == 1"));
    assert!(simple("$response.body#/items/0 == 'a'"));
}

#[test]
fn literals() {
    assert!(simple("true"));
    assert!(!simple("false"));
    assert!(!simple("null"));
    assert!(simple("$response.body.count == 4.0"));
    assert!(simple("-1 < 0"));
    assert!(simple("1.5e1 == 15"));
    assert!(simple("'it''s' == 'IT''S'"), "'' is a single quote inside a string");
}

/// 5.8.11.4.1: string comparisons are case-insensitive.
#[test]
fn strings_compare_case_insensitively() {
    assert!(simple("$response.body.status == 'ok'"));
    assert!(simple("$inputs.name != 'trinity'"));
    assert!(simple("'abc' < 'ABD'"));
}

/// Numeric strings are coerced to numbers by numeric operators.
#[test]
fn numeric_strings_are_coerced() {
    assert!(simple("$response.header.X-Count > 3"));
    assert!(simple("$response.body.price >= 19.5"));
    assert!(simple("'200' == 200"));
}

/// null only equals itself; ordering against null is false; `!=` against
/// null is true for any other value — how `data != null` in the spec's own
/// example reads.
#[test]
fn null_semantics() {
    assert!(simple("$response.body.empty == null"));
    assert!(simple("null == null"));
    assert!(!simple("$response.body.count == null"));
    assert!(simple("$response.body.count != null"));
    assert!(!simple("$response.body.empty < 1"));
    assert!(!simple("$response.body.empty > 1"));
    assert!(!simple("$response.body.empty"), "null fails");
}

/// Only `true` passes: a bare non-boolean value is not truthy.
#[test]
fn bare_values() {
    assert!(simple("$response.body.ok"));
    assert!(!simple("$response.body.count"));
}

/// 5.8.11.4.5: any evaluation error is a fail, never a panic or a pass.
#[test]
fn evaluation_errors_fail() {
    for c in ["$statusCode ==", "$nope == 1", "$inputs.missing == 1", "(1 == 1", "1 === 1", "'unterminated == 1"] {
        assert!(!simple(c), "{c}");
        assert!(condition::evaluate(&Criterion::simple(c), &scope()).is_err(), "{c}: the error must be visible");
    }
}

#[test]
fn regex_conditions() {
    let s = scope();
    assert!(condition::passes(&crit("{ context: $statusCode, condition: '^2\\d{2}$', type: regex }"), &s));
    assert!(!condition::passes(&crit("{ context: $statusCode, condition: '^4', type: regex }"), &s));
    assert!(condition::passes(&crit("{ context: $response.body.status, condition: 'K', type: regex }"), &s), "search, not a full match");
    assert!(
        condition::passes(&crit("{ context: $response.body.status, condition: '(?i)^({$inputs.statusPattern})$', type: regex }"), &s),
        "embedded expression is substituted before compilation"
    );
    assert!(!condition::passes(&crit("{ context: $response.body.empty, condition: '.*', type: regex }"), &s), "null fails");
    assert!(!condition::passes(&crit("{ context: $statusCode, condition: '([', type: regex }"), &s), "invalid regex fails");
}

#[test]
fn jsonpath_rfc9535() {
    let s = scope_with_body(json!({"pets": [{"name": "a", "status": "active"}], "none": []}));
    let jp = |c: &str| condition::passes(&crit(&format!("{{ context: $response.body, condition: '{c}', type: jsonpath }}")), &s);
    assert!(jp("$.pets[*]"));
    assert!(!jp("$.none[*]"), "empty nodelist fails");
    assert!(jp("$.pets[?@.status == \"active\"]"));
    // RFC 9535 count(): child array pets is non-empty, none is empty
    assert!(jp("$[?count(@.*) > 0]"));
    assert!(!jp("$.none[?count(@.*) > 0]"));
    assert!(!jp("$.pets[?@.status == \"gone\"]"));
    assert!(!jp("$..[[[bad"), "syntax error fails");
    assert!(
        condition::passes(
            &crit("{ context: $response.body, condition: '$.pets[?(@.status == \"{$inputs.expected}\")]', type: jsonpath }"),
            &s
        ),
        "embedded expression"
    );
}

#[test]
fn jsonpath_on_null_context_fails() {
    let s = scope_with_body(Value::Null);
    assert!(!condition::passes(&crit("{ context: $response.body, condition: '$', type: jsonpath }"), &s));
}

#[cfg(feature = "xpath")]
const XML: &str = "<root><pets><pet>a</pet><pet>b</pet></pets><items><item><price>12</price></item></items><empty/></root>";

#[cfg(feature = "xpath")]
fn xpath(c: &str, version: Option<&str>) -> bool {
    let s = scope_with_body(json!(XML));
    let t = match version {
        None => "xpath".to_string(),
        Some(v) => format!("{{ type: xpath, version: {v} }}"),
    };
    condition::passes(&crit(&format!("{{ context: $response.body, condition: \"{c}\", type: {t} }}")), &s)
}

#[test]
#[cfg(feature = "xpath")]
fn xpath_effective_boolean_value() {
    assert!(xpath("count(/root/pets/*) > 0", None), "boolean");
    assert!(xpath("/root/pets", None), "non-empty node-set");
    assert!(!xpath("/root/missing", None), "empty node-set");
    assert!(xpath("count(/root/pets/pet)", None), "non-zero number");
    assert!(!xpath("count(/root/missing)", None), "zero");
    assert!(xpath("string(/root/pets/pet[1])", None), "non-empty string");
    assert!(!xpath("string(/root/empty)", None), "empty string");
}

/// The version picks the engine: `string-join` exists in 2.0+, not in 1.0,
/// where it is an evaluation error and therefore a fail.
#[test]
#[cfg(feature = "xpath")]
fn xpath_versions() {
    let q = "string-join(/root/pets/pet, ',') = 'a,b'";
    for v in ["xpath-31", "xpath-30", "xpath-20"] {
        assert!(xpath(q, Some(v)), "{v}");
    }
    assert!(!xpath(q, Some("xpath-10")));
    assert!(xpath("count(/root/pets/pet) = 2", Some("xpath-10")));
}

#[test]
fn xpath_on_invalid_xml_fails() {
    let s = scope_with_body(json!("<not closed"));
    assert!(!condition::passes(&crit("{ context: $response.body, condition: '/a', type: xpath }"), &s));
}

/// 5.8.11.4.6: several criteria are an AND.
#[test]
fn multiple_criteria_are_all_required() {
    let s = scope();
    let both = [crit("{ condition: '$statusCode == 200' }"), crit("{ context: $response.body, condition: '$.data', type: jsonpath }")];
    assert!(condition::all(&both, &s));
    let one_fails = [crit("{ condition: '$statusCode == 200' }"), crit("{ condition: '$statusCode == 201' }")];
    assert!(!condition::all(&one_fails, &s));
    assert!(condition::all(&[], &s), "empty list passes");
}
