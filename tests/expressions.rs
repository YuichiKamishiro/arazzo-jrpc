//! Runtime expressions (spec 5.9): every ABNF form parses, evaluates against
//! a scope, keeps its type, embeds into strings by the 5.9 rules, and
//! `$sourceDescriptions` resolves with the 5.9.2 priority.

mod common;

use arazzo_jrpc::expr::{self, Scope};
use arazzo_jrpc::transport::{Exchange, Message};
use serde_json::{json, Value};

fn scope() -> Scope {
    let mut s = Scope { inputs: json!({"username": "neo", "user": {"name": "Neo", "tags": ["a", "b"]}, "x-api-key": "k", "a.b": 7}), ..Default::default() };
    s.set_output("bar", json!({"name": "wf-out"}));
    s.set_step_output("someStep", "pets", json!([{"id": 1}, {"id": 2}]));
    s.set_workflow_input("foo", "username", json!("trinity"));
    s.set_workflow_output("foo", "bar", json!({"x": [10, 20]}));
    s.exchange = Some(Exchange {
        url: "https://api.example.com/v1/pets/7?q=1".into(),
        method: "GET".into(),
        request_headers: vec![("Accept".into(), "application/json".into())],
        request_query: vec![("q".into(), "1".into())],
        request_path: vec![("id".into(), "7".into())],
        request_body: Some(json!({"user": {"uuid": "u-1"}})),
        status: 200,
        response_headers: vec![("Server".into(), "nginx".into()), ("X-Rate-Limit".into(), "10".into())],
        response_body: json!({"status": "ok", "items": [{"id": 5}], "a/b": 1, "m~n": 2}),
    });
    s.message = Some(Message {
        channel: "orders".into(),
        headers: vec![("Correlation".into(), "c-1".into())],
        payload: json!({"orderId": "o-9", "status": "confirmed"}),
    });
    s
}

fn eval(e: &str) -> Value {
    expr::evaluate(e, &scope()).unwrap_or_else(|err| panic!("{e}: {err}"))
}

#[test]
fn every_abnf_form_parses() {
    for e in [
        "$url", "$method", "$statusCode", "$self",
        "$request.header.accept", "$request.query.id", "$request.path.id",
        "$request.body", "$request.body#/user/uuid",
        "$response.header.Server", "$response.body", "$response.body#/status",
        "$message.header.Server", "$message.payload", "$message.payload#/status",
        "$inputs.username", "$inputs.user#/name", "$inputs.x-api-key",
        "$outputs.bar", "$outputs.bar#/name",
        "$steps.someStep.outputs.pets", "$steps.someStep.outputs.pets#/0/id",
        "$workflows.foo.inputs.username", "$workflows.foo.outputs.bar#/x/1",
        "$sourceDescriptions.petstore.getPetById", "$sourceDescriptions.petstore.url",
        "$components.parameters.foo", "$components.successActions.bar", "$components.failureActions.baz",
    ] {
        expr::parse(e).unwrap_or_else(|err| panic!("{e}: {err}"));
    }
}

#[test]
fn malformed_expressions_are_rejected() {
    for e in [
        "$", "$foo", "$steps.a", "$steps.a.pets", "$steps.a.b.outputs.c",
        "$request.cookie.x", "$response.status", "$workflows.foo.steps",
        "$workflows.foo.bar.x", "$components.inputs.x", "$inputs.", "$response.body#x",
        "$response.body#/a~2b",
    ] {
        assert!(expr::parse(e).is_err(), "{e} must be rejected");
    }
}

#[test]
fn values_keep_their_type() {
    assert_eq!(eval("$statusCode"), json!(200));
    assert_eq!(eval("$inputs.user#/tags"), json!(["a", "b"]));
    assert_eq!(eval("$response.body#/items/0/id"), json!(5));
    assert_eq!(eval("$steps.someStep.outputs.pets#/1/id"), json!(2));
    assert_eq!(eval("$workflows.foo.outputs.bar#/x/1"), json!(20));
    assert_eq!(eval("$outputs.bar#/name"), json!("wf-out"));
    assert_eq!(eval("$workflows.foo.inputs.username"), json!("trinity"));
    assert_eq!(eval("$inputs.a.b"), json!(7), "input name may contain dots");
}

#[test]
fn http_exchange_fields() {
    assert_eq!(eval("$url"), json!("https://api.example.com/v1/pets/7?q=1"));
    assert_eq!(eval("$method"), json!("GET"));
    assert_eq!(eval("$request.header.accept"), json!("application/json"));
    assert_eq!(eval("$request.query.q"), json!("1"));
    assert_eq!(eval("$request.path.id"), json!("7"));
    assert_eq!(eval("$request.body#/user/uuid"), json!("u-1"));
}

/// Header names are tokens: case-insensitive (RFC 9110); `name` is not.
#[test]
fn headers_are_case_insensitive() {
    assert_eq!(eval("$response.header.server"), json!("nginx"));
    assert_eq!(eval("$response.header.x-rate-limit"), json!("10"));
    assert_eq!(eval("$message.header.correlation"), json!("c-1"));
    assert!(expr::evaluate("$inputs.Username", &scope()).is_err(), "inputs are case-sensitive");
}

#[test]
fn json_pointer_escapes() {
    assert_eq!(eval("$response.body#/a~1b"), json!(1));
    assert_eq!(eval("$response.body#/m~0n"), json!(2));
}

/// Spec examples use property access after a body: `$message.payload.orderId`,
/// `$response.body.count`.
#[test]
fn property_dereference_after_bodies() {
    assert_eq!(eval("$message.payload.orderId"), json!("o-9"));
    assert_eq!(eval("$response.body.items[0].id"), json!(5));
    assert_eq!(eval("$inputs.user.tags[1]"), json!("b"));
}

#[test]
fn missing_values_are_errors_with_the_name() {
    let s = scope();
    for (e, needle) in [
        ("$inputs.nope", "nope"),
        ("$steps.ghost.outputs.x", "ghost"),
        ("$steps.someStep.outputs.nope", "nope"),
        ("$response.body#/missing", "/missing"),
        ("$workflows.unrun.outputs.x", "unrun"),
    ] {
        let err = expr::evaluate(e, &s).unwrap_err().to_string();
        assert!(err.contains(needle), "{e}: {err}");
    }
}

/// 5.9: scalars become their string form; parsed structures become JSON;
/// a value that is already a string is embedded as-is.
#[test]
fn embedding_rules() {
    let s = scope();
    let i = |t: &str| expr::interpolate(t, &s).unwrap();
    assert_eq!(
        i("https://{$inputs.username}/api/{$steps.someStep.outputs.pets#/0/id}/status"),
        json!("https://neo/api/1/status")
    );
    assert_eq!(i("n={$statusCode}"), json!("n=200"));
    assert_eq!(i("t={$inputs.user#/tags}"), json!(r#"t=["a","b"]"#));
    assert_eq!(i("o={$message.payload}"), json!(r#"o={"orderId":"o-9","status":"confirmed"}"#));
    assert_eq!(i("plain text"), json!("plain text"));
    let mut xml = scope();
    xml.set_step_output("fetch", "doc", json!("<a>1</a>"));
    assert_eq!(expr::interpolate("x{$steps.fetch.outputs.doc}", &xml).unwrap(), json!("x<a>1</a>"));
}

#[test]
fn a_whole_expression_value_is_not_stringified() {
    assert_eq!(expr::interpolate("$inputs.user#/tags", &scope()).unwrap(), json!(["a", "b"]));
    assert_eq!(expr::interpolate("$statusCode", &scope()).unwrap(), json!(200));
}

#[test]
fn unclosed_or_empty_braces_are_errors() {
    assert!(expr::interpolate("a {$inputs.username", &scope()).is_err());
    assert!(expr::interpolate("a {} b", &scope()).is_err());
}

/// 5.9.2: `$sourceDescriptions.<name>.<ref>` — operationId (or workflowId
/// for arazzo sources) first, then Source Description fields.
#[test]
fn source_description_resolution_priority() {
    let d = common::describe(&format!(
        "{}workflows: [ {{ workflowId: w, steps: [ {{ stepId: s, operationId: findPets }} ] }} ]\n",
        common::HEADER
    ));
    let s = Scope::for_description(&d);
    let op = expr::evaluate("$sourceDescriptions.petstore.getPetById", &s).unwrap();
    assert_eq!(op["operationId"], "getPetById");
    assert_eq!(
        expr::evaluate("$sourceDescriptions.petstore.url", &s).unwrap(),
        json!(common::PETSTORE_URL),
        "url is resolved relative to the document"
    );
    assert_eq!(expr::evaluate("$sourceDescriptions.petstore.type", &s).unwrap(), json!("openapi"));
}

#[test]
fn operation_id_named_like_a_field_wins() {
    let mut src = arazzo_jrpc::Sources::new();
    src.insert(
        common::ENTRY,
        &format!("{}workflows: [ {{ workflowId: w, steps: [ {{ stepId: s, operationId: url }} ] }} ]\n", common::HEADER),
    )
    .unwrap();
    src.insert(
        common::PETSTORE_URL,
        r#"{"openapi":"3.1.0","info":{"title":"t","version":"1"},"paths":{"/u":{"get":{"operationId":"url"}}}}"#,
    )
    .unwrap();
    let d = arazzo_jrpc::Description::load(&src, common::ENTRY).unwrap();
    let v = expr::evaluate("$sourceDescriptions.petstore.url", &Scope::for_description(&d)).unwrap();
    assert_eq!(v["operationId"], "url");
}

#[test]
fn components_and_self() {
    let d = common::describe(&format!(
        "$self: https://example.com/canonical.arazzo.yaml\n{}\
         workflows: [ {{ workflowId: w, steps: [ {{ stepId: s, operationId: findPets }} ] }} ]\n\
         components:\n  parameters: {{ foo: {{ name: foo, in: query, value: 1 }} }}\n\
         \x20 successActions: {{ bar: {{ name: bar, type: end }} }}\n",
        common::HEADER
    ));
    let s = Scope::for_description(&d);
    assert_eq!(expr::evaluate("$self", &s).unwrap(), json!("https://example.com/canonical.arazzo.yaml"));
    assert_eq!(expr::evaluate("$components.parameters.foo", &s).unwrap()["name"], "foo");
    assert_eq!(expr::evaluate("$components.successActions.bar", &s).unwrap()["type"], "end");
    assert!(expr::evaluate("$components.failureActions.none", &s).is_err());
}
