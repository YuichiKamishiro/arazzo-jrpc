//! Arazzo over JSON-RPC 2.0: an opaque source (no `type`) whose `Invoker`
//! speaks JSON-RPC over HTTP, the way a test rig's control ports do.
//!
//! The example is self-contained: it starts a tiny JSON-RPC server on a
//! loopback port in a thread, then runs the workflow against it twice. A
//! JSON-RPC `error` object becomes `CallFailure.body`, so `onFailure`
//! criteria can branch on `$response.body#/code`.
//!
//! ```sh
//! cargo run --example jsonrpc
//! ```

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use arazzo_jrpc::transport::{Call, CallFailure, Invoker};
use arazzo_jrpc::{Description, Done, Engine};
use serde_json::{json, Value};

const FLOW: &str = "
arazzo: 1.1.0
info: { title: calculator, version: '1' }
sourceDescriptions:
  - { name: calc, url: ./calc.openrpc.json }
workflows:
  - workflowId: average
    inputs:
      type: object
      required: [ a, b, n ]
      properties:
        a: { type: number }
        b: { type: number }
        n: { type: number }
    steps:
      - stepId: add
        operationId: $sourceDescriptions.calc.math.add
        requestBody: { payload: { a: $inputs.a, b: $inputs.b } }
        outputs: { sum: $response.body#/sum }
      - stepId: div
        operationId: $sourceDescriptions.calc.math.div
        requestBody: { payload: { a: $steps.add.outputs.sum, b: $inputs.n } }
        onFailure:
          - name: divisionByZero
            type: end
            criteria: [ { condition: $response.body#/code == -32602 } ]
        outputs: { quotient: $response.body#/quotient }
    outputs:
      sum: $steps.add.outputs.sum
      average: $steps.div.outputs.quotient
";

/// JSON-RPC 2.0 over HTTP for one source: `operation` is the method,
/// the step's `requestBody.payload` is `params`.
struct JsonRpc {
    url: String,
    client: reqwest::blocking::Client,
    next_id: u64,
}

impl JsonRpc {
    fn new(addr: &str) -> Self {
        Self { url: format!("http://{addr}/"), client: reqwest::blocking::Client::new(), next_id: 1 }
    }
}

impl Invoker for JsonRpc {
    fn invoke(&mut self, call: &Call) -> Result<Value, CallFailure> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": call.operation,
            "params": call.payload.clone().unwrap_or_else(|| json!({})),
        });
        let reply: Value = self
            .client
            .post(&self.url)
            .timeout(call.timeout.unwrap_or(Duration::from_secs(10)))
            .header("Content-Type", "application/json")
            .body(request.to_string())
            .send()
            .and_then(|r| r.text())
            .map_err(|e| CallFailure::new(format!("{}: {e}", call.source.name), Value::Null))
            .and_then(|t| serde_json::from_str(&t).map_err(|e| CallFailure::new(format!("{}: not JSON: {e}", call.source.name), Value::Null)))?;
        if reply["id"] != json!(id) {
            return Err(CallFailure::new(format!("reply id {} for request {id}", reply["id"]), Value::Null));
        }
        match reply.get("error") {
            // onFailure criteria see the error object as $response.body.
            Some(error) => Err(CallFailure::new(error["message"].as_str().unwrap_or("error").to_string(), error.clone())),
            None => Ok(reply["result"].clone()),
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = serve_calculator()?;
    println!("calculator listens on {addr}");

    let description = Description::parse_at("file:///calc/arazzo.yaml", FLOW)?;
    assert!(description.check().is_empty(), "{:?}", description.check());
    let mut rpc = JsonRpc::new(&addr);
    let mut report = |d: &Done| match (&d.failed, &d.action) {
        (Some(why), Some(action)) => println!("  step {} failed: {why}; action {action}", d.step_id),
        (Some(why), None) => println!("  step {} failed: {why}", d.step_id),
        (None, _) => println!("  step {} ok, outputs {}", d.step_id, json!(d.outputs)),
    };

    for n in [2.0, 0.0] {
        println!("average of 1 and 2 over n={n}:");
        let out = Engine::new(&description)
            .with_invoker("calc", &mut rpc)
            .on_step(&mut report)
            .run("average", json!({ "a": 1, "b": 2, "n": n }))?;
        println!("  workflow outputs: {}", json!(out.outputs));
    }
    Ok(())
}

// ---------------------------------------------------------------- the server

/// A JSON-RPC 2.0 server over HTTP/1.1 on a loopback port; returns its address.
fn serve_calculator() -> std::io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = handle(stream);
        }
    });
    Ok(addr)
}

fn handle(mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let reply = dispatch(&request);
    let text = reply.to_string();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    )
}

fn dispatch(request: &Value) -> Value {
    let id = request["id"].clone();
    let params = &request["params"];
    let (a, b) = (params["a"].as_f64().unwrap_or(0.0), params["b"].as_f64().unwrap_or(0.0));
    match request["method"].as_str() {
        Some("math.add") => json!({ "jsonrpc": "2.0", "id": id, "result": { "sum": a + b } }),
        Some("math.div") if b == 0.0 => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32602, "message": "division by zero", "data": { "a": a } }
        }),
        Some("math.div") => json!({ "jsonrpc": "2.0", "id": id, "result": { "quotient": a / b } }),
        other => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32601, "message": format!("no method {other:?}") }
        }),
    }
}
