//! Workflow Object (spec 5.8.4): inputs validated as JSON Schema 2020-12
//! (with $ref into components.inputs), outputs, workflow-level parameters,
//! workflow steps, workflow-level dependsOn, `$workflows.*` references and
//! reusable parameters.

mod common;

use arazzo_jrpc::Engine;
use common::*;
use serde_json::json;

fn doc(workflows: &str) -> arazzo_jrpc::Description {
    describe(&format!("{HEADER}workflows:\n{workflows}"))
}

#[test]
fn inputs_are_validated_by_json_schema() {
    let d = doc(
        "  - workflowId: w\n    inputs:\n      type: object\n      required: [ user ]\n      properties:\n\
         \x20       user: { type: object, required: [ id ], properties: { id: { type: integer, minimum: 1 }, tags: { type: array, items: { type: string }, maxItems: 2 } } }\n\
         \x20       mail: { type: string, pattern: '^[^@]+@[^@]+$' }\n\
         \x20   steps: [ { stepId: s, operationId: findPets } ]\n",
    );
    let run = |i: serde_json::Value| {
        let mut http = MockHttp::with(vec![json_response(200, json!([]))]);
        Engine::new(&d).with_http(&mut http).run("w", i)
    };
    run(json!({"user": {"id": 1, "tags": ["a"]}, "mail": "a@b"})).unwrap();
    // The schema cases themselves are covered by inputs.rs; this is the engine plumbing.
    let e = run(json!({"user": {"id": 0}})).unwrap_err().to_string();
    assert!(e.contains("minimum"), "{e}");
}

#[test]
fn inputs_ref_into_components_by_pointer_and_anchor() {
    let d = describe(&format!(
        "{HEADER}workflows:\n  - workflowId: w\n    inputs:\n      type: object\n      properties:\n\
         \x20       page: {{ $ref: '#/components/inputs/pagination' }}\n\
         \x20       who: {{ $ref: '#person' }}\n\
         \x20   steps: [ {{ stepId: s, operationId: findPets }} ]\n\
         components:\n  inputs:\n\
         \x20   pagination: {{ type: object, properties: {{ page: {{ type: integer, format: int32 }} }}, required: [ page ] }}\n\
         \x20   person: {{ $anchor: person, type: string, minLength: 2 }}\n"
    ));
    let run = |i: serde_json::Value| {
        let mut http = MockHttp::with(vec![json_response(200, json!([]))]);
        Engine::new(&d).with_http(&mut http).run("w", i)
    };
    run(json!({"page": {"page": 1}, "who": "neo"})).unwrap();
    // Pointer and $anchor resolution are covered by inputs.rs; this checks that components reach the engine.
    assert!(run(json!({"page": {}})).is_err());
}

#[test]
fn workflow_outputs_use_steps_and_selectors() {
    let d = doc(
        "  - workflowId: w\n    outputs:\n      id: $steps.s.outputs.id\n\
         \x20     email: { context: $steps.s.outputs.user, selector: $.profile.email, type: jsonpath }\n\
         \x20   steps:\n      - { stepId: s, operationId: findPets, outputs: { id: $response.body#/id, user: $response.body#/user } }\n",
    );
    let mut http = MockHttp::with(vec![json_response(200, json!({"id": 4, "user": {"profile": {"email": "m@x"}}}))]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(out.outputs["id"], 4);
    assert_eq!(out.outputs["email"], "m@x");
}

#[test]
fn a_workflow_step_maps_parameters_to_inputs_and_exposes_outputs() {
    let d = doc(
        "  - workflowId: parent\n    outputs: { name: $steps.child.outputs.name }\n    steps:\n\
         \x20     - { stepId: child, workflowId: getPet, parameters: [ { name: id, value: 7 } ], outputs: { name: $outputs.name } }\n\
         \x20     - { stepId: after, operationId: findPets, parameters: [ { name: status, in: query, value: $workflows.getPet.outputs.name } ] }\n\
         \x20 - workflowId: getPet\n    inputs: { type: object, required: [ id ], properties: { id: { type: integer } } }\n\
         \x20   outputs: { name: $steps.g.outputs.name }\n\
         \x20   steps: [ { stepId: g, operationId: getPetById, parameters: [ { name: petId, in: path, value: $inputs.id } ], outputs: { name: $response.body#/name } } ]\n",
    );
    let mut http = MockHttp::with(vec![json_response(200, json!({"name": "rex"})), json_response(200, json!([]))]);
    let out = Engine::new(&d).with_http(&mut http).run("parent", json!({})).unwrap();
    assert_eq!(http.seen[0].url, "https://api.example.com/v1/pets/7");
    assert_eq!(http.seen[1].url, "https://api.example.com/v1/pets?status=rex");
    assert_eq!(out.outputs["name"], "rex");
}

#[test]
fn workflow_level_parameters_apply_and_steps_override_by_name_and_in() {
    let d = doc(
        "  - workflowId: w\n    parameters:\n      - { name: X-Trace, in: header, value: wf }\n      - { name: status, in: query, value: wf }\n\
         \x20   steps:\n      - { stepId: a, operationId: findPets }\n\
         \x20     - { stepId: b, operationId: findPets, parameters: [ { name: status, in: query, value: step } ] }\n",
    );
    let mut http = MockHttp::with(vec![json_response(200, json!([])), json_response(200, json!([]))]);
    Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert!(http.seen[0].url.ends_with("?status=wf"));
    assert!(http.seen[0].headers.contains(&("X-Trace".into(), "wf".into())));
    assert!(http.seen[1].url.ends_with("?status=step"), "step overrides");
    assert!(http.seen[1].headers.contains(&("X-Trace".into(), "wf".into())), "but keeps the rest");
}

#[test]
fn reusable_parameters_with_value_override() {
    let d = describe(&format!(
        "{HEADER}workflows:\n  - workflowId: w\n    steps:\n\
         \x20     - {{ stepId: a, operationId: findPets, parameters: [ {{ reference: $components.parameters.status }} ] }}\n\
         \x20     - {{ stepId: b, operationId: findPets, parameters: [ {{ reference: $components.parameters.status, value: sold }} ] }}\n\
         components: {{ parameters: {{ status: {{ name: status, in: query, value: available }} }} }}\n"
    ));
    let mut http = MockHttp::with(vec![json_response(200, json!([])), json_response(200, json!([]))]);
    Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert!(http.seen[0].url.ends_with("?status=available"));
    assert!(http.seen[1].url.ends_with("?status=sold"));
}

/// Workflow-level dependsOn: prerequisites run first, once per session, and
/// their outputs are visible as `$workflows.<id>.outputs.*`.
#[test]
fn workflow_depends_on_runs_prerequisites_once() {
    let d = doc(
        "  - workflowId: login\n    outputs: { token: $steps.l.outputs.token }\n\
         \x20   steps: [ { stepId: l, operationId: loginUser, outputs: { token: $response.body#/token } } ]\n\
         \x20 - workflowId: a\n    dependsOn: [ login ]\n\
         \x20   steps: [ { stepId: x, operationId: findPets, parameters: [ { name: X-Trace, in: header, value: $workflows.login.outputs.token } ] } ]\n\
         \x20 - workflowId: b\n    dependsOn: [ login, a ]\n    steps: [ { stepId: y, operationId: findPets } ]\n",
    );
    let mut http = MockHttp::with(vec![
        json_response(200, json!({"token": "t1"})),
        json_response(200, json!([])),
        json_response(200, json!([])),
    ]);
    let mut session = Engine::new(&d).with_http(&mut http).session();
    session.run("b", json!({})).unwrap();
    assert_eq!(http.seen.len(), 3, "login and a once each");
    assert!(http.seen[1].headers.contains(&("X-Trace".into(), "t1".into())));
}

#[test]
fn workflow_dependency_cycles_and_unknowns_are_load_errors() {
    let cyc = try_describe(&format!(
        "{HEADER}workflows:\n  - {{ workflowId: a, dependsOn: [ b ], steps: [ {{ stepId: s, operationId: findPets }} ] }}\n\
         \x20 - {{ workflowId: b, dependsOn: [ a ], steps: [ {{ stepId: s, operationId: findPets }} ] }}\n"
    ));
    assert!(cyc.unwrap_err().to_string().contains("cycle"));
    let unknown = try_describe(&format!(
        "{HEADER}workflows: [ {{ workflowId: a, dependsOn: [ ghost ], steps: [ {{ stepId: s, operationId: findPets }} ] }} ]\n"
    ));
    assert!(unknown.unwrap_err().to_string().contains("ghost"));
}

#[test]
fn workflow_steps_must_reference_existing_workflows_without_cycles() {
    let e = try_describe(&format!(
        "{HEADER}workflows: [ {{ workflowId: a, steps: [ {{ stepId: s, workflowId: nope }} ] }} ]\n"
    ));
    assert!(e.unwrap_err().to_string().contains("nope"));
    let d = doc("  - { workflowId: a, steps: [ { stepId: s, workflowId: a } ] }\n");
    let mut http = MockHttp::default();
    let e = Engine::new(&d).with_http(&mut http).run("a", json!({})).unwrap_err();
    assert!(e.to_string().contains("recursive") || e.to_string().contains("depth"), "{e}");
}

/// A forward `$steps` reference cannot be satisfied sequentially (5.8.5.2.5).
#[test]
fn forward_step_references_are_reported() {
    let d = doc(
        "  - workflowId: w\n    steps:\n\
         \x20     - { stepId: a, operationId: findPets, parameters: [ { name: status, in: query, value: $steps.b.outputs.s } ] }\n\
         \x20     - { stepId: b, operationId: findPets, outputs: { s: $response.body#/s } }\n",
    );
    assert!(d.check().iter().any(|p| p.to_string().contains("$steps.b")));
}

/// Explicit `dependsOn` on a step that comes later (or on itself) cannot be
/// satisfied by a sequential run either (5.8.5.2.4) — a load error, not a
/// failure halfway through the run.
#[test]
fn depends_on_a_later_step_is_reported() {
    let d = doc(
        "  - workflowId: w\n    steps:\n\
         \x20     - { stepId: a, operationId: findPets, dependsOn: [ b ] }\n\
         \x20     - { stepId: b, operationId: findPets, dependsOn: [ b ] }\n",
    );
    let problems: Vec<String> = d.check().iter().map(|p| p.to_string()).collect();
    assert!(problems.iter().any(|p| p.contains("step a") && p.contains("dependsOn b")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("step b") && p.contains("dependsOn b")), "self-dependency: {problems:?}");
}

/// An implicit dependency (`$steps.b`) on a later step is reported even when
/// the workflow also uses `dependsOn` — the old check skipped such workflows.
#[test]
fn forward_references_are_reported_next_to_depends_on() {
    let d = doc(
        "  - workflowId: w\n    steps:\n\
         \x20     - { stepId: a, operationId: findPets }\n\
         \x20     - { stepId: b, operationId: findPets, dependsOn: [ a ], parameters: [ { name: status, in: query, value: $steps.c.outputs.s } ] }\n\
         \x20     - { stepId: c, operationId: findPets, outputs: { s: $response.body#/s } }\n",
    );
    assert!(d.check().iter().any(|p| p.to_string().contains("$steps.c")), "{:?}", d.check());
}

/// Dependencies on earlier steps are fine; a step's own actions may read its
/// own outputs (they are recorded before the actions run).
#[test]
fn backward_dependencies_are_clean() {
    let d = doc(
        "  - workflowId: w\n    steps:\n\
         \x20     - { stepId: a, operationId: findPets, outputs: { s: $response.body#/s }, onSuccess: [ { name: e, type: end, criteria: [ { condition: $steps.a.outputs.s == 'x' } ] } ] }\n\
         \x20     - { stepId: b, operationId: findPets, dependsOn: [ a ], parameters: [ { name: status, in: query, value: $steps.a.outputs.s } ] }\n",
    );
    assert!(d.check().is_empty(), "{:?}", d.check());
}

/// With `goto` the run order is not the array order, so the order is left to
/// the run-time check (`dependsOn` not yet satisfied → the step fails).
#[test]
fn workflows_with_goto_are_checked_at_run_time_only() {
    let d = doc(
        "  - workflowId: w\n    steps:\n\
         \x20     - { stepId: a, operationId: findPets, onSuccess: [ { name: j, type: goto, stepId: c } ] }\n\
         \x20     - { stepId: b, operationId: findPets, dependsOn: [ c ] }\n\
         \x20     - { stepId: c, operationId: findPets }\n",
    );
    assert!(d.check().is_empty(), "{:?}", d.check());
}

#[test]
fn runs_are_reported_step_by_step() {
    let d = doc("  - workflowId: w\n    steps:\n      - { stepId: a, operationId: findPets }\n      - { stepId: b, operationId: findPets }\n");
    let mut http = MockHttp::with(vec![json_response(200, json!([])), json_response(200, json!([]))]);
    let mut seen = vec![];
    let mut on_step = |d: &arazzo_jrpc::Done| seen.push(format!("{}/{}", d.workflow_id, d.step_id));
    Engine::new(&d).with_http(&mut http).on_step(&mut on_step).run("w", json!({})).unwrap();
    assert_eq!(seen, ["w/a", "w/b"]);
}

#[test]
fn dry_run_calls_nothing_and_still_checks_references() {
    let d = doc("  - workflowId: w\n    steps:\n      - { stepId: a, operationId: findPets, outputs: { x: $response.body#/x } }\n      - { stepId: b, operationId: getPetById, parameters: [ { name: petId, in: path, value: $steps.a.outputs.x } ] }\n");
    let out = Engine::new(&d).dry_run().run("w", json!({})).unwrap();
    assert_eq!(out.steps.len(), 2);
}
