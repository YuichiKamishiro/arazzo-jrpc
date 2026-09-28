//! Success and Failure Actions (spec 5.8.7, 5.8.8), workflow-level defaults
//! (5.8.4 successActions/failureActions) and Reusable Objects pointing at
//! components.

mod common;

use std::time::Duration;

use arazzo_jrpc::Engine;
use common::*;
use serde_json::json;

fn doc(workflows: &str) -> arazzo_jrpc::Description {
    describe(&format!("{HEADER}workflows:\n{workflows}"))
}

fn ok(body: serde_json::Value) -> arazzo_jrpc::transport::HttpResponse {
    json_response(200, body)
}

fn code(c: u16) -> arazzo_jrpc::transport::HttpResponse {
    json_response(c, json!({}))
}

fn trace(out: &arazzo_jrpc::Outcome) -> Vec<String> {
    out.steps.iter().map(|d| d.step_id.clone()).collect()
}

const OK200: &str = "successCriteria: [ { condition: $statusCode == 200 } ]";

#[test]
fn end_stops_and_returns_outputs() {
    let d = doc(&format!(
        "  - workflowId: w\n    outputs: {{ n: $steps.a.outputs.n }}\n    steps:\n\
         \x20     - {{ stepId: a, operationId: findPets, {OK200}, outputs: {{ n: $response.body#/n }}, onSuccess: [ {{ name: stop, type: end }} ] }}\n\
         \x20     - {{ stepId: b, operationId: findPets }}\n"
    ));
    let mut http = MockHttp::with(vec![ok(json!({"n": 3}))]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(trace(&out), ["a"]);
    assert_eq!(out.outputs["n"], 3);
    assert_eq!(out.steps[0].action.as_deref(), Some("stop"));
}

#[test]
fn first_matching_action_wins() {
    let d = doc(&format!(
        "  - workflowId: w\n    steps:\n\
         \x20     - stepId: a\n        operationId: findPets\n        {OK200}\n        onSuccess:\n\
         \x20         - {{ name: none, type: goto, stepId: c, criteria: [ {{ condition: $response.body#/0 == null }} ] }}\n\
         \x20         - {{ name: some, type: goto, stepId: b }}\n\
         \x20         - {{ name: never, type: end }}\n\
         \x20     - {{ stepId: b, operationId: findPets }}\n      - {{ stepId: c, operationId: findPets }}\n"
    ));
    let mut http = MockHttp::with(vec![ok(json!([{"id": 1}])), ok(json!([])), ok(json!([]))]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(trace(&out), ["a", "b", "c"]);
    assert_eq!(out.steps[0].action.as_deref(), Some("some"));
}

#[test]
fn goto_workflow_passes_parameters_as_inputs_and_does_not_return() {
    let d = doc(&format!(
        "  - workflowId: w\n    steps:\n\
         \x20     - {{ stepId: a, operationId: findPets, {OK200}, onSuccess: [ {{ name: jump, type: goto, workflowId: other, parameters: [ {{ name: id, value: 5 }} ] }} ] }}\n\
         \x20     - {{ stepId: skipped, operationId: findPets }}\n\
         \x20 - workflowId: other\n    inputs: {{ type: object, properties: {{ id: {{ type: integer }} }} }}\n\
         \x20   steps:\n      - {{ stepId: o, operationId: getPetById, parameters: [ {{ name: petId, in: path, value: $inputs.id }} ] }}\n"
    ));
    let mut http = MockHttp::with(vec![ok(json!([])), ok(json!({}))]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(trace(&out), ["a", "o"]);
    assert_eq!(http.seen[1].url, "https://api.example.com/v1/pets/5");
}

#[test]
fn failure_without_actions_breaks_and_returns_an_error() {
    let d = doc(&format!("  - workflowId: w\n    steps:\n      - {{ stepId: a, operationId: findPets, {OK200} }}\n      - {{ stepId: b, operationId: findPets }}\n"));
    let mut http = MockHttp::with(vec![code(500)]);
    let e = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap_err();
    assert!(e.to_string().contains("a"), "{e}");
    assert_eq!(http.seen.len(), 1);
}

#[test]
fn end_on_failure_returns_normally() {
    let d = doc(&format!(
        "  - workflowId: w\n    steps:\n      - {{ stepId: a, operationId: findPets, {OK200}, onFailure: [ {{ name: give_up, type: end }} ] }}\n      - {{ stepId: b, operationId: findPets }}\n"
    ));
    let mut http = MockHttp::with(vec![code(404)]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(trace(&out), ["a"]);
    assert!(out.steps[0].failed.is_some());
}

/// retryLimit absent → a single retry: two attempts in total.
#[test]
fn retry_defaults_to_one_retry() {
    let d = doc(&format!("  - workflowId: w\n    steps:\n      - {{ stepId: a, operationId: findPets, {OK200}, onFailure: [ {{ name: again, type: retry }} ] }}\n"));
    let mut http = MockHttp::with(vec![code(503), code(503), code(200)]);
    assert!(Engine::new(&d).with_http(&mut http).run("w", json!({})).is_err());
    assert_eq!(http.seen.len(), 2);
}

#[test]
fn retry_limit_and_retry_after() {
    let d = doc(&format!(
        "  - workflowId: w\n    steps:\n      - {{ stepId: a, operationId: findPets, {OK200}, \
         onFailure: [ {{ name: again, type: retry, retryAfter: 0.25, retryLimit: 3, criteria: [ {{ condition: $statusCode == 503 }} ] }} ] }}\n"
    ));
    let mut http = MockHttp::with(vec![code(503), code(503), code(503), code(200)]);
    let mut slept = vec![];
    let mut sleep = |d: Duration| slept.push(d);
    Engine::new(&d).with_http(&mut http).with_sleeper(&mut sleep).run("w", json!({})).unwrap();
    assert_eq!(http.seen.len(), 4);
    assert_eq!(slept, [Duration::from_millis(250); 3]);
}

/// An HTTP Retry-After header overrules retryAfter (spec 5.8.8.1).
#[test]
fn retry_after_header_wins() {
    let d = doc(&format!(
        "  - workflowId: w\n    steps:\n      - {{ stepId: a, operationId: findPets, {OK200}, onFailure: [ {{ name: again, type: retry, retryAfter: 9 }} ] }}\n"
    ));
    let mut http = MockHttp::with(vec![response_with_headers(503, &[("Retry-After", "2")], json!({})), code(200)]);
    let mut slept = vec![];
    let mut sleep = |d: Duration| slept.push(d);
    Engine::new(&d).with_http(&mut http).with_sleeper(&mut sleep).run("w", json!({})).unwrap();
    assert_eq!(slept, [Duration::from_secs(2)]);
}

/// retryLimit is exhausted before subsequent failure actions run.
#[test]
fn exhausted_retry_falls_through_to_the_next_action() {
    let d = doc(&format!(
        "  - workflowId: w\n    steps:\n\
         \x20     - {{ stepId: a, operationId: findPets, {OK200}, onFailure: [ {{ name: again, type: retry, retryLimit: 2 }}, {{ name: fallback, type: goto, stepId: b }} ] }}\n\
         \x20     - {{ stepId: b, operationId: findPets }}\n"
    ));
    let mut http = MockHttp::with(vec![code(500), code(500), code(500), ok(json!([]))]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(http.seen.len(), 4);
    assert_eq!(out.steps.last().unwrap().step_id, "b");
    assert!(out.steps.iter().any(|s| s.action.as_deref() == Some("fallback")));
}

/// retry + stepId: run that step, then retry the current one.
#[test]
fn retry_via_another_step() {
    let d = doc(&format!(
        "  - workflowId: w\n    steps:\n\
         \x20     - {{ stepId: call, operationId: findPets, {OK200}, onSuccess: [ {{ name: done, type: end }} ], \
         onFailure: [ {{ name: relogin, type: retry, stepId: login, criteria: [ {{ condition: $statusCode == 401 }} ] }} ] }}\n\
         \x20     - {{ stepId: login, operationId: loginUser }}\n"
    ));
    let mut http = MockHttp::with(vec![code(401), ok(json!({})), ok(json!([]))]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(trace(&out), ["call", "login", "call"]);
}

/// retry + workflowId: run that workflow (e.g. refresh a token), come back.
#[test]
fn retry_via_another_workflow() {
    let d = doc(&format!(
        "  - workflowId: w\n    steps:\n\
         \x20     - {{ stepId: call, operationId: findPets, {OK200}, onFailure: [ {{ name: refresh, type: retry, workflowId: refresh, criteria: [ {{ condition: $statusCode == 401 }} ] }} ] }}\n\
         \x20 - workflowId: refresh\n    steps: [ {{ stepId: token, operationId: loginUser }} ]\n"
    ));
    let mut http = MockHttp::with(vec![code(401), ok(json!({})), ok(json!([]))]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(trace(&out), ["call", "token", "call"]);
}

/// Workflow-level actions apply to every step; a step action with the same
/// name overrides one, it cannot remove the rest.
#[test]
fn workflow_level_actions_are_inherited_and_overridable() {
    let d = doc(&format!(
        "  - workflowId: w\n    failureActions:\n\
         \x20     - {{ name: again, type: retry, retryLimit: 1 }}\n\
         \x20   successActions:\n      - {{ name: finish, type: end, criteria: [ {{ condition: $response.body#/last == true }} ] }}\n\
         \x20   steps:\n\
         \x20     - {{ stepId: a, operationId: findPets, {OK200} }}\n\
         \x20     - {{ stepId: b, operationId: findPets, {OK200}, onFailure: [ {{ name: again, type: end }} ] }}\n\
         \x20     - {{ stepId: c, operationId: findPets }}\n"
    ));
    // a: 500 → inherited retry → 200 {last:false}; b: 500 → overridden again=end.
    let mut http = MockHttp::with(vec![code(500), ok(json!({"last": false})), code(500)]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(trace(&out), ["a", "a", "b"]);

    let mut http = MockHttp::with(vec![ok(json!({"last": true}))]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(trace(&out), ["a"], "inherited successAction fired");
}

#[test]
fn reusable_actions_from_components() {
    let d = describe(&format!(
        "{HEADER}workflows:\n  - workflowId: w\n    steps:\n\
         \x20     - {{ stepId: a, operationId: findPets, {OK200}, onFailure: [ {{ reference: $components.failureActions.again }} ], onSuccess: [ {{ reference: $components.successActions.stop }} ] }}\n\
         \x20     - {{ stepId: b, operationId: findPets }}\n\
         components:\n  successActions: {{ stop: {{ name: stop, type: end }} }}\n\
         \x20 failureActions: {{ again: {{ name: again, type: retry, retryLimit: 1 }} }}\n"
    ));
    let mut http = MockHttp::with(vec![code(500), ok(json!([]))]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(trace(&out), ["a", "a"]);
}

#[test]
fn unknown_references_are_load_errors() {
    let e = try_describe(&format!(
        "{HEADER}workflows: [ {{ workflowId: w, steps: [ {{ stepId: a, operationId: findPets, onSuccess: [ {{ reference: $components.successActions.ghost }} ] }} ] }} ]\n"
    ))
    .unwrap_err();
    assert!(e.to_string().contains("ghost"), "{e}");
}

#[test]
fn a_goto_cycle_is_bounded() {
    let d = doc("  - workflowId: w\n    steps:\n      - { stepId: a, operationId: findPets, onSuccess: [ { name: again, type: goto, stepId: a } ] }\n");
    let mut http = MockHttp::with((0..2000).map(|_| ok(json!([]))).collect());
    let e = Engine::new(&d).with_http(&mut http).with_step_limit(50).run("w", json!({})).unwrap_err();
    assert!(e.to_string().contains("50"), "{e}");
}
