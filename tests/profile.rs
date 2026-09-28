//! What a JSON-RPC rig needs from the engine, with no `x-*` semantics in the
//! crate: an invoker for sources the spec has no `type` for. `x-*` fields are
//! carried, never acted on; the crate invents no input value.

use std::time::Duration;

use arazzo_jrpc::transport::{Call, CallFailure, Invoker};
use arazzo_jrpc::{Description, Engine, Sources};
use serde_json::{json, Map, Value};

const ENTRY: &str = "file:///rig/stand/arazzo.yaml";

fn describe(workflows: &str) -> Description {
    let text = format!(
        "arazzo: 1.1.0\ninfo: {{ title: rig, version: '1' }}\n\
         sourceDescriptions:\n  - {{ name: pu, url: '../emulator_pu_rust/openrpc.json', x-protocol: openrpc, x-addr: '127.0.0.1:13010' }}\n\
         workflows:\n{workflows}"
    );
    let mut s = Sources::new();
    s.insert(ENTRY, &text).unwrap();
    Description::load(&s, ENTRY).unwrap()
}

/// Records calls; `target.mark` fails once with exec=17 in the error data.
#[derive(Default)]
struct Rpc {
    seen: Vec<(String, String, Map<String, Value>)>,
    marks: u32,
}

impl Invoker for Rpc {
    fn invoke(&mut self, call: &Call) -> Result<Value, CallFailure> {
        let params = call.payload.as_ref().and_then(Value::as_object).cloned().unwrap_or_default();
        self.seen.push((call.source.name.clone(), call.operation.clone(), params));
        if call.operation == "target.mark" {
            self.marks += 1;
            if self.marks == 1 {
                return Err(CallFailure::new("exec=17", json!({"code": -32000, "data": {"exec": 17}})));
            }
        }
        Ok(json!({"ok": true, "objno": 1}))
    }
}

/// The stand's mark automaton, spec-shaped: params go in requestBody,
/// the JSON-RPC error object is `$response.body` for onFailure.
const MARK: &str = "  - workflowId: mark\n    inputs: { type: object, properties: { number: { type: string } } }\n    steps:\n\
    \x20     - { stepId: ksl, operationId: $sourceDescriptions.pu.ksl.create, requestBody: { payload: { group: 1 } } }\n\
    \x20     - stepId: mark\n        operationId: $sourceDescriptions.pu.target.mark\n\
    \x20       requestBody: { payload: { number: $inputs.number } }\n\
    \x20       onSuccess: [ { name: marked, type: end } ]\n\
    \x20       onFailure:\n          - { name: ksl_gone, type: goto, stepId: ksl, criteria: [ { condition: $response.body#/data/exec == 17 } ] }\n";

#[test]
fn custom_invoker_serves_untyped_sources() {
    let d = describe(MARK);
    assert!(d.check().is_empty(), "{:?}", d.check());
    let mut rpc = Rpc::default();
    let out = Engine::new(&d).with_invoker("pu", &mut rpc).run("mark", json!({"number": "79261234567"})).unwrap();
    let ops: Vec<&str> = rpc.seen.iter().map(|(_, op, _)| op.as_str()).collect();
    assert_eq!(ops, ["ksl.create", "target.mark", "ksl.create", "target.mark"]);
    assert_eq!(rpc.seen[1].2["number"], "79261234567");
    assert_eq!(out.steps.last().unwrap().action.as_deref(), Some("marked"));
}

#[test]
fn a_source_without_a_transport_is_an_error_naming_it() {
    let d = describe(MARK);
    let e = Engine::new(&d).run("mark", json!({"number": "1"})).unwrap_err();
    assert!(e.to_string().contains("pu"), "{e}");
}

/// `x-…` operationIds are ordinary names: with one source they go to its invoker.
#[test]
fn x_operation_ids_are_not_builtins() {
    let d = describe("  - workflowId: w\n    steps: [ { stepId: pause, operationId: x-sleep, requestBody: { payload: { ms: 5 } } } ]\n");
    let mut rpc = Rpc::default();
    Engine::new(&d).with_invoker("pu", &mut rpc).run("w", json!({})).unwrap();
    assert_eq!(rpc.seen, [("pu".to_string(), "x-sleep".to_string(), json!({"ms": 5}).as_object().cloned().unwrap())]);
}

#[test]
fn x_fields_on_steps_do_nothing() {
    let d = describe("  - workflowId: w\n    steps: [ { stepId: s, operationId: $sourceDescriptions.pu.status.get, x-settle-ms: 200 } ]\n");
    let mut rpc = Rpc::default();
    let mut slept = vec![];
    let mut sleep = |d: Duration| slept.push(d);
    Engine::new(&d).with_invoker("pu", &mut rpc).with_sleeper(&mut sleep).run("w", json!({})).unwrap();
    assert!(slept.is_empty(), "{slept:?}");
}

/// A child workflow gets what the parent's `parameters` pass plus its own
/// `default`s — nothing else; `x-generate` is carried, never acted on.
#[test]
fn child_inputs_are_parameters_and_defaults_only() {
    let d = describe(
        "  - workflowId: parent\n    inputs: { type: object, properties: { cin: { type: string } } }\n\
         \x20   steps: [ { stepId: c, workflowId: child, parameters: [ { name: cin, value: $inputs.cin } ] } ]\n\
         \x20 - workflowId: child\n    inputs:\n      type: object\n      properties:\n\
         \x20       cin: { type: string, pattern: '^[0-9]{8}$', x-generate: cin }\n\
         \x20       raw: { type: string, default: '{root}/voice.raw' }\n\
         \x20   steps: [ { stepId: s, operationId: $sourceDescriptions.pu.x3.start, requestBody: { payload: { cin: $inputs.cin, raw: $inputs.raw } } } ]\n",
    );
    let mut rpc = Rpc::default();
    Engine::new(&d).with_invoker("pu", &mut rpc).run("parent", json!({"cin": "01020304"})).unwrap();
    assert_eq!(rpc.seen[0].2["cin"], "01020304");
    assert_eq!(rpc.seen[0].2["raw"], "{root}/voice.raw", "default as is");

    let mut rpc = Rpc::default();
    Engine::new(&d).with_invoker("pu", &mut rpc).run("parent", json!({})).unwrap();
    assert!(!rpc.seen[0].2.contains_key("cin"), "an unset input is neither passed nor invented");
}
