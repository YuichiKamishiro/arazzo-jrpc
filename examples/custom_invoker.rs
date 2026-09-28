//! An opaque source (no `type`) served by an in-process `Invoker`, with a
//! step trace. The first `target.mark` call fails and `onFailure` routes the
//! workflow back to `ksl.create` before trying again.
//!
//! ```sh
//! cargo run --example custom_invoker
//! ```

use arazzo_jrpc::transport::{Call, CallFailure, Invoker};
use arazzo_jrpc::{Description, Done, Engine};
use serde_json::{json, Value};

const FLOW: &str = "
arazzo: 1.1.0
info: { title: rig, version: '1' }
sourceDescriptions:
  - { name: pu, url: ./pu.openrpc.json }
workflows:
  - workflowId: mark
    inputs:
      type: object
      required: [ number ]
      properties:
        number: { type: string }
    steps:
      - { stepId: ksl, operationId: $sourceDescriptions.pu.ksl.create }
      - stepId: mark
        operationId: $sourceDescriptions.pu.target.mark
        requestBody: { payload: { number: $inputs.number } }
        onFailure:
          - name: ksl_gone
            type: goto
            stepId: ksl
            criteria: [ { condition: $response.body#/data/exec == 17 } ]
        outputs: { objno: $response.body#/objno }
    outputs: { objno: $steps.mark.outputs.objno }
";

/// Plays a JSON-RPC server: `target.mark` fails once with exec=17.
#[derive(Default)]
struct Rpc {
    marked_once: bool,
}

impl Invoker for Rpc {
    fn invoke(&mut self, call: &Call) -> Result<Value, CallFailure> {
        println!("  -> {} {}", call.operation, call.payload.as_ref().map(|p| p.to_string()).unwrap_or_default());
        if call.operation == "target.mark" && !self.marked_once {
            self.marked_once = true;
            // onFailure criteria see this body as $response.body.
            return Err(CallFailure::new("exec=17", json!({ "code": -32000, "data": { "exec": 17 } })));
        }
        Ok(json!({ "objno": 1 }))
    }
}

fn main() -> Result<(), arazzo_jrpc::Error> {
    let description = Description::parse_at("file:///rig/arazzo.yaml", FLOW)?;
    assert!(description.check().is_empty(), "an opaque source needs no document");

    let mut rpc = Rpc::default();
    let mut report = |d: &Done| match (&d.failed, &d.action) {
        (Some(why), Some(action)) => println!("step {} failed ({why}), action {action}", d.step_id),
        (Some(why), None) => println!("step {} failed ({why})", d.step_id),
        (None, _) => println!("step {} ok", d.step_id),
    };
    let out = Engine::new(&description)
        .with_invoker("pu", &mut rpc)
        .on_step(&mut report)
        .run("mark", json!({ "number": "79261234567" }))?;
    println!("workflow outputs: {}", json!(out.outputs));
    Ok(())
}
