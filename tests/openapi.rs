//! OpenAPI steps: operation resolution (operationId bare / prefixed,
//! operationPath), request building (server URL, path templating, query,
//! querystring, header, cookie, request body and replacements) and the
//! response side ($statusCode, headers, body parsing).

mod common;

use arazzo_jrpc::Engine;
use common::*;
use serde_json::json;

fn flow(steps: &str) -> String {
    format!("{HEADER}workflows:\n  - workflowId: w\n    inputs: {{ type: object }}\n    steps:\n{steps}")
}

fn run_one(steps: &str, inputs: serde_json::Value, answer: arazzo_jrpc::transport::HttpResponse) -> (MockHttp, arazzo_jrpc::Outcome) {
    let d = describe(&flow(steps));
    let mut http = MockHttp::with(vec![answer]);
    let out = Engine::new(&d).with_http(&mut http).run("w", inputs).unwrap();
    (http, out)
}

#[test]
fn bare_operation_id_resolves_in_a_single_source_document() {
    let (http, _) = run_one("      - { stepId: s, operationId: findPets }\n", json!({}), json_response(200, json!([])));
    assert_eq!(http.seen[0].method, "GET");
    assert_eq!(http.seen[0].url, "https://api.example.com/v1/pets");
}

#[test]
fn prefixed_operation_id_and_operation_path() {
    let (http, _) = run_one(
        "      - { stepId: a, operationId: $sourceDescriptions.petstore.addPet }\n",
        json!({}),
        json_response(201, json!({})),
    );
    assert_eq!((http.seen[0].method.as_str(), http.seen[0].url.as_str()), ("POST", "https://api.example.com/v1/pets"));

    let (http, _) = run_one(
        "      - stepId: b\n        operationPath: '{$sourceDescriptions.petstore.url}#/paths/~1pets~1{petId}/delete'\n\
         \x20       parameters: [ { name: petId, in: path, value: 9 } ]\n",
        json!({}),
        json_response(204, json!(null)),
    );
    assert_eq!((http.seen[0].method.as_str(), http.seen[0].url.as_str()), ("DELETE", "https://api.example.com/v1/pets/9"));
}

#[test]
fn unknown_operations_are_reported_before_any_call() {
    let d = describe(&flow("      - { stepId: s, operationId: noSuchOp }\n"));
    let problems = d.check();
    assert!(problems.iter().any(|p| p.to_string().contains("noSuchOp")), "{problems:?}");
    let mut http = MockHttp::default();
    assert!(Engine::new(&d).with_http(&mut http).run("w", json!({})).is_err());
    assert!(http.seen.is_empty(), "no calls at all");

    let bad_path = describe(&flow("      - { stepId: s, operationPath: '{$sourceDescriptions.petstore.url}#/paths/~1nope/get' }\n"));
    assert!(bad_path.check().iter().any(|p| p.to_string().contains("~1nope")));
}

#[test]
fn bare_operation_id_is_ambiguous_with_several_sources() {
    let text = format!(
        "{HEADER}  - {{ name: other, url: ../specs/petstore.yaml, type: openapi }}\n\
         workflows: [ {{ workflowId: w, steps: [ {{ stepId: s, operationId: findPets }} ] }} ]\n"
    );
    let problems = describe(&text).check();
    assert!(problems.iter().any(|p| p.to_string().contains("$sourceDescriptions")), "{problems:?}");
}

#[test]
fn path_query_header_and_cookie_parameters() {
    let (http, _) = run_one(
        "      - stepId: s\n        operationId: getPetById\n        parameters:\n\
         \x20         - { name: petId, in: path, value: 'a b/c' }\n\
         \x20         - { name: status, in: query, value: 'sold out' }\n\
         \x20         - { name: limit, in: query, value: $inputs.limit }\n\
         \x20         - { name: X-Trace, in: header, value: 't-{$inputs.limit}' }\n\
         \x20         - { name: sid, in: cookie, value: s1 }\n\
         \x20         - { name: theme, in: cookie, value: dark }\n",
        json!({"limit": 5}),
        json_response(200, json!({})),
    );
    let r = &http.seen[0];
    assert_eq!(r.url, "https://api.example.com/v1/pets/a%20b%2Fc?status=sold%20out&limit=5");
    assert!(r.headers.contains(&("X-Trace".into(), "t-5".into())));
    assert!(r.headers.contains(&("Cookie".into(), "sid=s1; theme=dark".into())));
}

#[test]
fn querystring_parameter_is_the_whole_query() {
    let (http, _) = run_one(
        "      - stepId: s\n        operationId: search\n        parameters:\n\
         \x20         - { name: q, in: querystring, value: 'category={$inputs.cat}&inStock=true' }\n",
        json!({"cat": "dogs"}),
        json_response(200, json!([])),
    );
    assert_eq!(http.seen[0].url, "https://api.example.com/v1/search?category=dogs&inStock=true");
}

#[test]
fn json_payload_with_expressions_keeps_types() {
    let (http, _) = run_one(
        "      - stepId: s\n        operationId: addPet\n        requestBody:\n\
         \x20         contentType: application/json\n\
         \x20         payload: { petOrder: { petId: $inputs.id, status: placed, complete: false, tags: $inputs.tags } }\n",
        json!({"id": 7, "tags": ["x"]}),
        json_response(201, json!({})),
    );
    let r = &http.seen[0];
    assert!(r.headers.contains(&("Content-Type".into(), "application/json".into())));
    let body: serde_json::Value = serde_json::from_slice(r.body.as_ref().unwrap()).unwrap();
    assert_eq!(body, json!({"petOrder": {"petId": 7, "status": "placed", "complete": false, "tags": ["x"]}}));
}

#[test]
fn templated_string_and_whole_expression_payloads() {
    let (http, _) = run_one(
        "      - stepId: s\n        operationId: addPet\n        requestBody:\n\
         \x20         contentType: application/xml\n\
         \x20         payload: '<petOrder><petId>{$inputs.id}</petId></petOrder>'\n",
        json!({"id": 7}),
        json_response(201, json!({})),
    );
    assert_eq!(http.seen[0].body.as_deref(), Some(&b"<petOrder><petId>7</petId></petOrder>"[..]));

    let (http, _) = run_one(
        "      - stepId: s\n        operationId: addPet\n        requestBody: { contentType: application/json, payload: $inputs.order }\n",
        json!({"order": {"a": [1, 2]}}),
        json_response(201, json!({})),
    );
    assert_eq!(serde_json::from_slice::<serde_json::Value>(http.seen[0].body.as_ref().unwrap()).unwrap(), json!({"a": [1, 2]}));
}

#[test]
fn form_payload_is_url_encoded() {
    let (http, _) = run_one(
        "      - stepId: s\n        operationId: loginUser\n        requestBody:\n\
         \x20         contentType: application/x-www-form-urlencoded\n\
         \x20         payload: { client_id: $inputs.cid, scope: 'read write' }\n",
        json!({"cid": "c&1"}),
        json_response(200, json!({})),
    );
    assert_eq!(http.seen[0].body.as_deref(), Some(&b"client_id=c%261&scope=read%20write"[..]));
}

#[test]
#[cfg(feature = "xpath")]
fn replacements_by_pointer_jsonpath_and_xpath() {
    let (http, _) = run_one(
        "      - stepId: s\n        operationId: addPet\n        requestBody:\n\
         \x20         contentType: application/json\n\
         \x20         payload: { petId: 0, items: [ { sku: A, quantity: 1 }, { sku: B, quantity: 1 } ] }\n\
         \x20         replacements:\n\
         \x20           - { target: /petId, value: $inputs.id }\n\
         \x20           - { target: \"$.items[?(@.sku=='B')].quantity\", targetSelectorType: jsonpath, value: 5 }\n\
         \x20           - { target: /added, value: yes }\n",
        json!({"id": 3}),
        json_response(201, json!({})),
    );
    let body: serde_json::Value = serde_json::from_slice(http.seen[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["petId"], 3);
    assert_eq!(body["items"][1]["quantity"], 5);
    assert_eq!(body["added"], "yes");

    let (http, _) = run_one(
        "      - stepId: s\n        operationId: addPet\n        requestBody:\n\
         \x20         contentType: application/xml\n\
         \x20         payload: '<Envelope><Header><CustomerId>0</CustomerId></Header></Envelope>'\n\
         \x20         replacements:\n            - { target: /Envelope/Header/CustomerId, value: $inputs.cid }\n",
        json!({"cid": "C-9"}),
        json_response(201, json!({})),
    );
    let xml = String::from_utf8(http.seen[0].body.clone().unwrap()).unwrap();
    assert!(xml.contains("<CustomerId>C-9</CustomerId>"), "{xml}");
}

#[test]
fn selector_values_in_parameters_and_payloads() {
    let (http, _) = run_one(
        "      - stepId: s\n        operationId: getPetById\n        parameters:\n\
         \x20         - { name: petId, in: path, value: { context: $inputs.user, selector: $.pet.id, type: jsonpath } }\n",
        json!({"user": {"pet": {"id": 11}}}),
        json_response(200, json!({})),
    );
    assert_eq!(http.seen[0].url, "https://api.example.com/v1/pets/11");
}

#[test]
fn response_side_expressions() {
    let (_, out) = run_one(
        "      - stepId: s\n        operationId: findPets\n\
         \x20       successCriteria: [ { condition: $statusCode == 200 } ]\n\
         \x20       outputs:\n\
         \x20         first: $response.body#/0/id\n\
         \x20         limit: $response.header.x-rate-limit\n\
         \x20         url: $url\n          method: $method\n          code: $statusCode\n",
        json!({}),
        response_with_headers(200, &[("X-Rate-Limit", "10")], json!([{"id": 1}])),
    );
    let o = &out.steps[0].outputs;
    assert_eq!((o["first"].clone(), o["limit"].clone(), o["code"].clone()), (json!(1), json!("10"), json!(200)));
    assert_eq!((o["url"].clone(), o["method"].clone()), (json!("https://api.example.com/v1/pets"), json!("GET")));
}

#[test]
fn non_json_bodies_stay_strings() {
    let mut r = json_response(200, json!(null));
    r.headers = vec![("Content-Type".into(), "application/xml".into())];
    r.body = b"<a>1</a>".to_vec();
    let (_, out) = run_one("      - { stepId: s, operationId: findPets, outputs: { raw: $response.body } }\n", json!({}), r);
    assert_eq!(out.steps[0].outputs["raw"], json!("<a>1</a>"));
}

/// Without successCriteria any completed exchange is a success; with them
/// an unmet one fails the step.
#[test]
fn success_is_criteria_not_status() {
    let (_, out) = run_one("      - { stepId: s, operationId: findPets }\n", json!({}), json_response(500, json!({})));
    assert!(out.steps[0].failed.is_none());

    let d = describe(&flow("      - { stepId: s, operationId: findPets, successCriteria: [ { condition: $statusCode == 200 } ] }\n"));
    let mut http = MockHttp::with(vec![json_response(500, json!({}))]);
    let e = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap_err();
    assert!(e.to_string().contains("$statusCode == 200"), "{e}");
}

#[test]
fn step_timeout_is_passed_to_the_transport() {
    let (http, _) = run_one("      - { stepId: s, operationId: findPets, timeout: 1500 }\n", json!({}), json_response(200, json!([])));
    assert_eq!(http.seen[0].timeout, Some(std::time::Duration::from_millis(1500)));
}

#[test]
fn parameters_are_checked_against_the_operation() {
    let d = describe(&flow("      - { stepId: s, operationId: getPetById }\n"));
    assert!(d.check().iter().any(|p| p.to_string().contains("petId")), "required path parameter not given");
}
