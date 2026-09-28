# arazzo-jrpc

[OAI Arazzo 1.1.0](https://spec.openapis.org/arazzo/v1.1.0) (and 1.0.x) for
Rust: parsing against the official OAI JSON Schema (bundled in `schemas/`),
a semantic lint, multi-document descriptions (`$self`, Arazzo sources loaded
transitively), runtime expressions (the whole ABNF of section 5.9),
simple / regex / JSONPath / XPath criteria, selectors, `inputs` validated
by JSON Schema 2020-12, and a sequential executor over pluggable transports.

The crate does no I/O of its own and prints nothing: the caller brings the
document texts and decides what to report about each step.

## Installation

```sh
cargo add arazzo
```

Or, if you bring your own transport:

```sh
cargo add arazzo --no-default-features
```

## Quick start

An Arazzo document, the OpenAPI document it refers to, and a stand-in HTTP
transport that answers without touching the network:

```rust
use arazzo_jrpc::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};
use arazzo_jrpc::{Description, Engine, Sources};
use serde_json::json;

/// A custom HTTP transport: here a canned reply instead of the network.
struct Canned;

impl HttpTransport for Canned {
    fn send(&mut self, req: &HttpRequest) -> Result<HttpResponse, TransportError> {
        assert_eq!((req.method.as_str(), req.url.as_str()), ("GET", "https://api.example.com/v1/pets/7"));
        Ok(HttpResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: br#"{"id": 7, "name": "Rex"}"#.to_vec(),
        })
    }
}

const FLOW: &str = "
arazzo: 1.1.0
info: { title: pets, version: '1' }
sourceDescriptions:
  - { name: petstore, url: ./petstore.json, type: openapi }
workflows:
  - workflowId: getPet
    inputs: { type: object, required: [ id ], properties: { id: { type: integer } } }
    steps:
      - stepId: get
        operationId: getPetById
        parameters: [ { name: petId, in: path, value: $inputs.id } ]
        successCriteria: [ { condition: $statusCode == 200 } ]
        outputs: { name: $response.body#/name }
    outputs: { name: $steps.get.outputs.name }
";

let petstore = json!({
    "openapi": "3.1.0",
    "info": { "title": "petstore", "version": "1" },
    "servers": [{ "url": "https://api.example.com/v1" }],
    "paths": { "/pets/{petId}": { "get": {
        "operationId": "getPetById",
        "parameters": [{ "name": "petId", "in": "path", "required": true }]
    } } }
});

let mut sources = Sources::new();
sources.insert("file:///flows/pets.arazzo.yaml", FLOW)?;
sources.insert("file:///flows/petstore.json", &petstore.to_string())?;
let d = Description::load(&sources, "file:///flows/pets.arazzo.yaml")?;
assert!(d.check().is_empty(), "{:?}", d.check());

let mut http = Canned;
let out = Engine::new(&d).with_http(&mut http).run("getPet", json!({ "id": 7 }))?;
assert_eq!(out.outputs["name"], "Rex");
assert_eq!(out.steps.len(), 1);
# Ok::<(), arazzo_jrpc::Error>(())
```

With the `http` feature the built-in `transport::http::ReqwestHttp` replaces
`Canned` and the same workflow runs against a real server.

## Features

| Feature | What it adds | Default |
|---|---|---|
| `http` | `transport::http::ReqwestHttp`, a blocking HTTP transport for OpenAPI steps (reqwest, rustls) | yes |
| `xpath` | XPath criteria, selectors and replacements: 3.1/3.0/2.0 via xee, 1.0 via sxd | yes |

Without `xpath` an XPath criterion or selector is an evaluation error
(`XPath unavailable: crate built without the xpath feature`).

## Pipeline

```text
Sources::insert(uri, text)      // each document: Arazzo, OpenAPI or AsyncAPI
  → Description::load(&sources, entry)   // base URI, sources, every reference
  → Description::check()                 // Vec<Problem>: everything checkable without running
  → Engine::new(&d).with_*(…).run(workflowId, inputs)
  → Outcome { outputs, steps: Vec<Done> }
```

- `Sources::insert` parses YAML/JSON, tells the document kind by its root
  key (`arazzo` / `openapi` / `asyncapi`), validates Arazzo against the
  official schema and indexes `$self`. A single document without external
  sources: `Description::parse_at(uri, text)`.
- `Description::load` fails when a reference does not resolve
  (`$components.*`, `dependsOn`, a step or workflow named by an action). A
  missing API document is not a load error; `check()` reports it.
- `Description::check` lints the documents and reports operations and
  channels that do not exist, an `action` that contradicts the AsyncAPI
  operation, required OpenAPI parameters that are not passed, and step
  ordering: `dependsOn` and `$steps.X` must point at a step earlier in the
  array (in a workflow with `goto` this is checked at run time instead).
- `Engine::run` is one run; `Engine::session()` → `Session::run` is several
  runs sharing `$workflows.*` with workflow `dependsOn` executed once.
- A `Done` is produced for every attempt of a step, including a failure
  handled by `onFailure` (`failed: Some(…)`) and polling (`retrying: true`);
  an unhandled failure ends the run with the error `workflow w, step s: …`.

## Transports

A step goes to the transport of its source's kind; a missing transport is an
error before the first call, naming the source.

| Source | Trait | Wiring | Provided |
|---|---|---|---|
| OpenAPI (`type: openapi`) | `HttpTransport::send(&HttpRequest) → HttpResponse` | `Engine::with_http` | `transport::http::ReqwestHttp` (feature `http`) |
| AsyncAPI (`type: asyncapi`) | `MessageTransport::send(&Message)`, `receive(channel, correlation, timeout) → Option<Message>`, `subscribe(channel)` | `Engine::with_messaging` | `transport::InMemoryBroker` (for tests; `reply_on` is a "live" reply that is lost without a subscription) |
| no `type` (opaque) | `Invoker::invoke(&Call) → Value` | `Engine::with_invoker(name, …)` | none (for example your application's JSON-RPC client) |

- `HttpTransport` returns a response for any status (500 is a response, not
  an error); `TransportError` is only for "no response at all".
- `Invoker` receives `Call { source, operation, parameters, payload, timeout }`
  (`operation` without the `$sourceDescriptions.<name>.` prefix). Success is
  `$response.body`; `CallFailure { message, body }` fails the step, and
  `onFailure` criteria see `body` as `$response.body`.
- **`x-*` fields are not interpreted.** Specification extensions (5.10) are
  parsed into `extensions` and available to the caller, but do not affect
  execution; `operationId: x-…` is an ordinary operation name of its source.
- Workflow inputs are only what was passed (by the caller or by the
  `parameters` of a `workflowId` step) plus schema `default`s, then validated
  against the schema. The crate never invents values.
- Other knobs: `on_step(FnMut(&Done))`, `with_sleeper` (instead of
  `thread::sleep` for `retryAfter` and `Retry-After`), `with_step_limit`
  (500 attempts by default: `goto` can loop), `dry_run()` (the graph is
  walked without calls; responses and outputs are `null`).

An opaque source with a custom `Invoker`, where the first call fails and
`onFailure` routes back to an earlier step:

```rust
use arazzo_jrpc::transport::{Call, CallFailure, Invoker};
use arazzo_jrpc::{Description, Done, Engine};
use serde_json::{json, Value};

/// A source without `type`: params travel as `requestBody.payload`;
/// the first `target.mark` answers exec=17.
#[derive(Default)]
struct Rpc {
    calls: Vec<(String, Option<Value>)>,
    failed: bool,
}

impl Invoker for Rpc {
    fn invoke(&mut self, call: &Call) -> Result<Value, CallFailure> {
        self.calls.push((call.operation.clone(), call.payload.clone()));
        if call.operation == "target.mark" && !self.failed {
            self.failed = true;
            // onFailure criteria see this body as $response.body.
            return Err(CallFailure::new("exec=17", json!({ "code": -32000, "data": { "exec": 17 } })));
        }
        Ok(json!({ "objno": 1 }))
    }
}

let d = Description::parse_at("file:///rig/stand/arazzo.yaml", "
arazzo: 1.1.0
info: { title: rig, version: '1' }
sourceDescriptions:
  - { name: pu, url: ../pu/openrpc.json }
workflows:
  - workflowId: mark
    inputs:
      type: object
      properties:
        number: { type: string }
        cin: { type: string }
    steps:
      - { stepId: ksl, operationId: $sourceDescriptions.pu.ksl.create }
      - stepId: mark
        operationId: $sourceDescriptions.pu.target.mark
        requestBody: { payload: { number: $inputs.number, cin: $inputs.cin } }
        onFailure:
          - name: ksl_gone
            type: goto
            stepId: ksl
            criteria: [ { condition: $response.body#/data/exec == 17 } ]
        outputs: { objno: $response.body#/objno }
")?;
assert!(d.check().is_empty(), "an opaque source needs no document");

let mut rpc = Rpc::default();
let mut seen = Vec::new();
let mut report = |d: &Done| seen.push(format!("{}{}", d.step_id, if d.failed.is_some() { "!" } else { "" }));

let out = Engine::new(&d)
    .with_invoker("pu", &mut rpc)
    .on_step(&mut report)
    .run("mark", json!({ "number": "79261234567", "cin": "23120000" }))?;

assert_eq!(seen, ["ksl", "mark!", "ksl", "mark"]);
assert_eq!(rpc.calls[3].1, Some(json!({ "number": "79261234567", "cin": "23120000" })));
assert_eq!(out.steps.last().unwrap().outputs["objno"], 1);
# Ok::<(), arazzo_jrpc::Error>(())
```

## Examples

Runnable programs in [`examples/`](examples/):

| Example | Run | Shows |
|---|---|---|
| `validate` | `cargo run --example validate -- flow.arazzo.yaml api.yaml …` | load a description from files and print `check()` problems |
| `http` | `cargo run --example http` | run the bundled `examples/data/` workflow against <https://httpbin.org> with `ReqwestHttp` (needs the `http` feature and network) |
| `custom_invoker` | `cargo run --example custom_invoker` | an opaque source served by an in-process `Invoker`, with a step trace |
| `jsonrpc` | `cargo run --example jsonrpc` | an `Invoker` speaking JSON-RPC 2.0 over HTTP to a server the example starts itself; a JSON-RPC error drives `onFailure` (needs the `http` feature) |

## Decided semantics

Where the specification is silent or allows several readings, the crate
chose:

- Execution is sequential: step by step, in document order, until an action
  says otherwise.
- A step without `successCriteria` succeeds on any completed exchange,
  including HTTP 500; success is the criteria, not the status.
- `retry` without `retryLimit` is one more attempt; an exhausted `retry`
  yields to the next matching action; HTTP `Retry-After` beats `retryAfter`.
- An evaluation error in a criterion (missing value, JSONPath over `null`,
  broken XML) fails the criterion; it is not an exception.
- `simple` conditions: strings compare case-insensitively, numeric strings
  are coerced to numbers, `null` equals only `null`, `x != null` on a
  non-null value is true; a bare non-boolean value fails.
- Selector: one node is the value, several are an array, none is `null`.
- A step's `dependsOn` does not start the dependency (5.8.5.4). Array order
  is execution order, so explicit (`dependsOn`) and implicit (`$steps.X`)
  dependencies must point earlier in the array, otherwise `check()`
  reports it; in a workflow with `goto`/`retry` to a step this is checked at
  run time. A workflow `dependsOn` starts an unfinished dependency (with
  empty inputs) once per `Session`.
- Asynchrony lives inside sequential execution (the specification allows
  it): `receive` waits at its own position. So that a reply arriving while
  earlier steps run is not lost, the engine calls
  `MessageTransport::subscribe` for the channel of every `receive` step
  before the first step.
- A source without `type` is opaque: no document is needed, calls go to
  the `Invoker` registered for that source.
- A bare `channelPath` is a channel name in the only AsyncAPI source.
- An optional input that was neither passed nor defaulted is left out of
  the payload; the method applies its own default.
- Known deviation from the published 1.1 schema: `replacements[].value` may
  be any non-object (the specification's own example 5.8.15 uses `value: 10`).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

The official JSON Schemas in `schemas/` and the test documents in `tests/oai/`
come from the [OAI Arazzo Specification](https://github.com/OAI/Arazzo-Specification)
repository and stay under the Apache License, Version 2.0; see `schemas/NOTICE`
and `tests/oai/NOTICE` for the source and the one change made to a schema.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
