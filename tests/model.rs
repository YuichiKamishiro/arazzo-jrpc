//! Object model (spec 5.8): every object, its fixed fields, `x-*` on every
//! extensible object, naming rules, and what the step kinds are.

use arazzo_jrpc::model::{ActionKind, ParameterIn, StepTarget};
use arazzo_jrpc::Document;

const BASE: &str = "\
arazzo: 1.1.0
info: { title: t, version: '1', summary: s, description: d, x-info: 1 }
sourceDescriptions:
  - { name: api, url: api.yaml, type: openapi, x-src: 2 }
  - { name: bus, url: bus.yaml, type: asyncapi }
  - { name: flows, url: flows.arazzo.yaml, type: arazzo }
";

fn parse(workflows: &str) -> arazzo_jrpc::Result<Document> {
    Document::parse(&format!("{BASE}{workflows}"))
}

#[test]
fn root_fields_including_self_and_components() {
    let d = Document::parse(&format!(
        "$self: https://example.com/a.arazzo.yaml\n{BASE}\
         workflows: [ {{ workflowId: w, steps: [ {{ stepId: s, operationId: $sourceDescriptions.api.op }} ] }} ]\n\
         components: {{ inputs: {{ page: {{ type: integer }} }} }}\nx-root: true\n"
    ))
    .unwrap();
    assert_eq!(d.self_uri.as_deref(), Some("https://example.com/a.arazzo.yaml"));
    assert_eq!(d.info.summary.as_deref(), Some("s"));
    assert_eq!(d.info.extensions["x-info"], 1);
    assert_eq!(d.source_descriptions[0].extensions["x-src"], 2);
    assert_eq!(d.extensions["x-root"], true);
    assert!(d.components.as_ref().unwrap().inputs.contains_key("page"));
}

#[test]
fn source_description_type_is_optional_and_enumerated() {
    let ok = "arazzo: 1.1.0\ninfo: { title: t, version: '1' }\n\
              sourceDescriptions: [ { name: a, url: a.yaml } ]\n\
              workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op } ] } ]\n";
    Document::parse(ok).unwrap();
    let bad = ok.replace("url: a.yaml }", "url: a.yaml, type: graphql }");
    assert!(Document::parse(&bad).unwrap_err().to_string().contains("type"));
}

#[test]
fn names_follow_the_spec_patterns() {
    let src = "arazzo: 1.1.0\ninfo: { title: t, version: '1' }\n\
               sourceDescriptions: [ { name: 'bad name', url: a.yaml } ]\n\
               workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op } ] } ]\n";
    assert!(Document::parse(src).unwrap_err().to_string().contains("name"));

    let comp = format!(
        "{BASE}workflows: [ {{ workflowId: w, steps: [ {{ stepId: s, operationId: $sourceDescriptions.api.op }} ] }} ]\n\
         components: {{ parameters: {{ 'bad key!': {{ name: p, in: query, value: 1 }} }} }}\n"
    );
    assert!(Document::parse(&comp).unwrap_err().to_string().contains("bad key!"));

    let out = parse(
        "workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, \
         outputs: { 'no spaces': $statusCode } } ] } ]\n",
    );
    assert!(out.unwrap_err().to_string().contains("no spaces"));
}

#[test]
fn ids_are_unique() {
    let dup_wf = parse(
        "workflows:\n  - { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op } ] }\n  \
         - { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op } ] }\n",
    );
    assert!(dup_wf.unwrap_err().to_string().contains("workflowId"));
    let dup_step = parse(
        "workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.a }, \
         { stepId: s, operationId: $sourceDescriptions.api.b } ] } ]\n",
    );
    assert!(dup_step.unwrap_err().to_string().contains("stepId"));
}

#[test]
fn step_targets_are_mutually_exclusive_kinds() {
    let d = parse(
        "workflows:\n  - workflowId: w\n    steps:\n\
         \x20     - { stepId: a, operationId: $sourceDescriptions.api.getPet }\n\
         \x20     - { stepId: b, operationPath: '{$sourceDescriptions.api.url}#/paths/~1pets/get' }\n\
         \x20     - { stepId: c, channelPath: '{$sourceDescriptions.bus.url}#/channels/orders', action: receive }\n\
         \x20     - { stepId: d, workflowId: other }\n\
         \x20     - { stepId: e, workflowId: $sourceDescriptions.flows.remote }\n\
         \x20 - { workflowId: other, steps: [ { stepId: x, operationId: $sourceDescriptions.api.op } ] }\n",
    )
    .unwrap();
    let steps = &d.workflows[0].steps;
    assert!(matches!(steps[0].target, StepTarget::OperationId(_)));
    assert!(matches!(steps[1].target, StepTarget::OperationPath(_)));
    assert!(matches!(steps[2].target, StepTarget::ChannelPath(_)));
    assert!(matches!(steps[3].target, StepTarget::WorkflowId(_)));
    assert!(matches!(steps[4].target, StepTarget::WorkflowId(_)));
}

#[test]
fn async_fields_timeout_correlation_and_depends_on() {
    let d = parse(
        "workflows:\n  - workflowId: w\n    steps:\n\
         \x20     - { stepId: send, operationId: $sourceDescriptions.bus.placeOrder, action: send }\n\
         \x20     - stepId: recv\n        operationId: $sourceDescriptions.bus.confirmOrder\n\
         \x20       action: receive\n        correlationId: $inputs.cid\n        timeout: 6000\n\
         \x20       dependsOn: [ send ]\n",
    )
    .unwrap();
    let s = &d.workflows[0].steps[1];
    assert_eq!(s.timeout, Some(6000));
    assert_eq!(s.depends_on, ["send"]);
    assert!(s.correlation_id.is_some());
}

#[test]
fn parameter_locations_and_selector_values() {
    let d = parse(
        "workflows:\n  - workflowId: w\n    steps:\n      - stepId: s\n\
         \x20       operationId: $sourceDescriptions.api.op\n        parameters:\n\
         \x20         - { name: a, in: path, value: 1 }\n\
         \x20         - { name: b, in: query, value: x }\n\
         \x20         - { name: c, in: querystring, value: 'k=v' }\n\
         \x20         - { name: d, in: header, value: h }\n\
         \x20         - { name: e, in: cookie, value: c }\n\
         \x20         - { name: f, in: query, value: { context: $inputs.user, selector: $.id, type: jsonpath } }\n\
         \x20         - { reference: $components.parameters.page, value: 2 }\n\
         components: { parameters: { page: { name: page, in: query, value: 1 } } }\n",
    )
    .unwrap();
    let p = &d.workflows[0].steps[0].parameters;
    assert_eq!(p.len(), 7);
    assert_eq!(p[0].as_parameter().unwrap().location, Some(ParameterIn::Path));
    assert_eq!(p[2].as_parameter().unwrap().location, Some(ParameterIn::Querystring));
    assert!(p[5].as_parameter().unwrap().value.is_selector());
    assert!(p[6].as_reference().is_some());
}

#[test]
fn actions_and_their_kinds() {
    let d = parse(
        "workflows:\n  - workflowId: w\n    steps:\n      - stepId: s\n\
         \x20       operationId: $sourceDescriptions.api.op\n\
         \x20       onSuccess:\n          - { name: a, type: end }\n\
         \x20         - { name: b, type: goto, stepId: s }\n\
         \x20         - { name: c, type: goto, workflowId: w, parameters: [ { name: x, value: 1 } ] }\n\
         \x20       onFailure:\n          - { name: d, type: retry, retryAfter: 1.5, retryLimit: 3 }\n\
         \x20         - { name: e, type: retry, stepId: s }\n\
         \x20         - { reference: $components.failureActions.refresh }\n\
         components: { failureActions: { refresh: { name: refresh, type: retry, workflowId: w } } }\n",
    )
    .unwrap();
    let s = &d.workflows[0].steps[0];
    let kinds: Vec<ActionKind> = s.on_success.iter().filter_map(|a| a.as_action()).map(|a| a.kind).collect();
    assert_eq!(kinds, [ActionKind::End, ActionKind::Goto, ActionKind::Goto]);
    assert_eq!(s.on_failure[0].as_action().unwrap().retry_after, Some(1.5));
}

#[test]
fn goto_needs_exactly_one_target_and_parameters_need_workflow() {
    let no_target = parse(
        "workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, \
         onSuccess: [ { name: a, type: goto } ] } ] } ]\n",
    );
    assert!(no_target.unwrap_err().to_string().contains("goto"));
    let both = parse(
        "workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, \
         onSuccess: [ { name: a, type: goto, stepId: s, workflowId: w } ] } ] } ]\n",
    );
    assert!(both.is_err());
    let params_without_wf = parse(
        "workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, \
         onSuccess: [ { name: a, type: goto, stepId: s, parameters: [ { name: x, value: 1 } ] } ] } ] } ]\n",
    );
    assert!(params_without_wf.unwrap_err().to_string().contains("parameters"));
}

#[test]
fn retry_fields_are_non_negative() {
    let bad = parse(
        "workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, \
         onFailure: [ { name: r, type: retry, retryLimit: -1 } ] } ] } ]\n",
    );
    assert!(bad.unwrap_err().to_string().contains("retryLimit"));
}

#[test]
fn criterion_type_forms() {
    let d = parse(
        "workflows:\n  - workflowId: w\n    steps:\n      - stepId: s\n\
         \x20       operationId: $sourceDescriptions.api.op\n        successCriteria:\n\
         \x20         - condition: $statusCode == 200\n\
         \x20         - { context: $statusCode, condition: '^2', type: regex }\n\
         \x20         - { context: $response.body, condition: '$.a', type: jsonpath }\n\
         \x20         - { context: $response.body, condition: '/a', type: xpath }\n\
         \x20         - { context: $response.body, condition: '$.a', type: { type: jsonpath, version: draft-goessner-dispatch-jsonpath-00 } }\n\
         \x20         - { context: $response.body, condition: '/a', type: { type: xpath, version: xpath-10 } }\n",
    )
    .unwrap();
    assert_eq!(d.workflows[0].steps[0].success_criteria.len(), 6);
    let missing_context = parse(
        "workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, \
         successCriteria: [ { condition: '^2', type: regex } ] } ] } ]\n",
    );
    assert!(missing_context.unwrap_err().to_string().contains("context"));
    let bad_version = parse(
        "workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, \
         successCriteria: [ { context: $response.body, condition: '$', type: { type: jsonpath, version: xpath-10 } } ] } ] } ]\n",
    );
    assert!(bad_version.unwrap_err().to_string().contains("version"));
}

#[test]
fn request_body_and_replacements() {
    let d = parse(
        "workflows:\n  - workflowId: w\n    steps:\n      - stepId: s\n\
         \x20       operationId: $sourceDescriptions.api.op\n        requestBody:\n\
         \x20         contentType: application/json\n          payload: { a: 1 }\n\
         \x20         replacements:\n            - { target: /a, value: $inputs.a }\n\
         \x20           - { target: $.a, targetSelectorType: jsonpath, value: '2' }\n\
         \x20           - { target: /x, targetSelectorType: { type: xpath, version: xpath-30 }, value: '3' }\n",
    )
    .unwrap();
    let rb = d.workflows[0].steps[0].request_body.as_ref().unwrap();
    assert_eq!(rb.content_type.as_deref(), Some("application/json"));
    assert_eq!(rb.replacements.len(), 3);
}

#[test]
fn workflow_level_defaults_and_outputs() {
    let d = parse(
        "workflows:\n  - workflowId: w\n    dependsOn: [ other ]\n\
         \x20   parameters: [ { name: k, in: header, value: v } ]\n\
         \x20   successActions: [ { name: done, type: end } ]\n\
         \x20   failureActions: [ { name: again, type: retry } ]\n\
         \x20   outputs: { id: $steps.s.outputs.id, sel: { context: $response.body, selector: /a, type: jsonpointer } }\n\
         \x20   steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, outputs: { id: $response.body#/id } } ]\n\
         \x20 - { workflowId: other, steps: [ { stepId: x, operationId: $sourceDescriptions.api.op } ] }\n",
    )
    .unwrap();
    let w = &d.workflows[0];
    assert_eq!(w.depends_on, ["other"]);
    assert_eq!(w.outputs.len(), 2);
    assert_eq!(w.success_actions.len(), 1);
}

#[test]
fn reusable_object_ignores_unknown_properties() {
    // «This object cannot be extended ... any properties added MUST be ignored»
    parse(
        "workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, \
         parameters: [ { reference: $components.parameters.p } ] } ] } ]\n\
         components: { parameters: { p: { name: p, in: query, value: 1 } } }\n",
    )
    .unwrap();
}

#[test]
fn unknown_fixed_fields_are_rejected_but_extensions_are_not() {
    let unknown = parse(
        "workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, retries: 3 } ] } ]\n",
    );
    assert!(unknown.unwrap_err().to_string().contains("retries"));
    parse(
        "workflows: [ { workflowId: w, x-w: 1, steps: [ { stepId: s, operationId: $sourceDescriptions.api.op, x-s: 1, \
         onSuccess: [ { name: a, type: end, x-a: 1 } ], successCriteria: [ { condition: 'true', x-c: 1 } ] } ] } ]\n",
    )
    .unwrap();
}
