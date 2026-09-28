//! The executor: runs workflows of a [`Description`] step by step over the
//! caller's transports, following `onSuccess`/`onFailure` actions.
//!
//! Sequential by design: one step at a time, in document order unless an
//! action jumps. Every attempt comes back as a [`Done`].

mod actions;
mod run;
mod step;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::description::Description;
use crate::error::{Error, Result};
use crate::expr::WorkflowState;
use crate::model::AsyncAction;
use crate::transport::{HttpTransport, Invoker, MessageTransport};

/// Steps per run unless [`Engine::with_step_limit`] says otherwise.
const DEFAULT_STEP_LIMIT: usize = 500;

/// One executed attempt of a step, successful or not.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Done {
    pub workflow_id: String,
    pub step_id: String,
    /// The target as written, without the `$sourceDescriptions.` prefix.
    pub operation: String,
    pub outputs: Map<String, Value>,
    /// The action that fired, when one did.
    pub action: Option<String>,
    /// Why the attempt failed; a failure handled by an action is still a `Done`.
    pub failed: Option<String>,
    /// The action that fired is a `retry` — callers may hide such polling noise.
    pub retrying: bool,
}

/// The result of one run.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// Outputs of the workflow that was run.
    pub outputs: Map<String, Value>,
    /// Every attempt in execution order (prerequisites and child workflows included).
    pub steps: Vec<Done>,
}

/// Configures how a description is executed: transports, callbacks, limits.
pub struct Engine<'a> {
    desc: &'a Description,
    http: Option<&'a mut dyn HttpTransport>,
    messaging: Option<&'a mut dyn MessageTransport>,
    invokers: Vec<(String, &'a mut dyn Invoker)>,
    sleeper: Option<&'a mut dyn FnMut(Duration)>,
    on_step: Option<&'a mut dyn FnMut(&Done)>,
    step_limit: usize,
    dry: bool,
}

impl<'a> Engine<'a> {
    /// Starts configuring a run of `desc`.
    ///
    /// No transports, the default step limit (500) and real sleeping.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::{Description, Engine};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.pu.status } ] } ]
    /// "#)?;
    /// let out = Engine::new(&d).dry_run().run("w", json!({}))?;
    /// assert_eq!(out.steps[0].operation, "pu.status");
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(desc: &'a Description) -> Self {
        Engine {
            desc,
            http: None,
            messaging: None,
            invokers: Vec::new(),
            sleeper: None,
            on_step: None,
            step_limit: DEFAULT_STEP_LIMIT,
            dry: false,
        }
    }

    /// Uses `http` for steps on OpenAPI sources.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};
    /// use arazzo_jrpc::{Description, Engine, Sources};
    /// use serde_json::json;
    ///
    /// /// Answers every request with the path it saw.
    /// struct Echo;
    ///
    /// impl HttpTransport for Echo {
    ///     fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
    ///         let body = json!({"path": request.url.trim_start_matches("https://api.example.com")});
    ///         Ok(HttpResponse {
    ///             status: 200,
    ///             headers: vec![("Content-Type".into(), "application/json".into())],
    ///             body: serde_json::to_vec(&body).unwrap(),
    ///         })
    ///     }
    /// }
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let mut sources = Sources::new();
    /// sources.insert("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: api, url: api.json, type: openapi } ]
    /// workflows:
    ///   - workflowId: w
    ///     steps:
    ///       - stepId: get
    ///         operationId: getPetById
    ///         parameters: [ { name: petId, in: path, value: 7 } ]
    ///         successCriteria: [ { condition: $statusCode == 200 } ]
    ///         outputs: { path: $response.body#/path }
    /// "#)?;
    /// sources.insert("https://x/api.json", r#"{"openapi": "3.1.0", "info": {"title": "a", "version": "1"},
    ///     "servers": [{"url": "https://api.example.com/v1"}],
    ///     "paths": {"/pets/{petId}": {"get": {"operationId": "getPetById"}}}}"#)?;
    /// let d = Description::load(&sources, "https://x/flow.arazzo.yaml")?;
    /// let mut http = Echo;
    /// let out = Engine::new(&d).with_http(&mut http).run("w", json!({}))?;
    /// assert_eq!(out.steps[0].outputs["path"], "/v1/pets/7");
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_http(mut self, http: &'a mut dyn HttpTransport) -> Self {
        self.http = Some(http);
        self
    }

    /// Uses `messaging` for steps on AsyncAPI sources.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::{InMemoryBroker, Message};
    /// use arazzo_jrpc::{Description, Engine, Sources};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let mut sources = Sources::new();
    /// sources.insert("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: bus, url: bus.json, type: asyncapi } ]
    /// workflows:
    ///   - workflowId: w
    ///     steps:
    ///       - stepId: wait
    ///         operationId: $sourceDescriptions.bus.confirmOrder
    ///         action: receive
    ///         timeout: 50
    ///         outputs: { status: $message.payload.status }
    /// "#)?;
    /// sources.insert("https://x/bus.json", r##"{"asyncapi": "3.0.0", "info": {"title": "b", "version": "1"},
    ///     "channels": {"confirmations": {"address": "orders.confirmed"}},
    ///     "operations": {"confirmOrder": {"action": "receive", "channel": {"$ref": "#/channels/confirmations"}}}}"##)?;
    /// let d = Description::load(&sources, "https://x/flow.arazzo.yaml")?;
    /// let mut bus = InMemoryBroker::default();
    /// bus.queue(Message::new("orders.confirmed", json!({"status": "confirmed"})));
    /// let out = Engine::new(&d).with_messaging(&mut bus).run("w", json!({}))?;
    /// assert_eq!(out.steps[0].outputs["status"], "confirmed");
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_messaging(mut self, messaging: &'a mut dyn MessageTransport) -> Self {
        self.messaging = Some(messaging);
        self
    }

    /// Serves operations of the untyped source named `source` with `invoker`.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::{Call, CallFailure, Invoker};
    /// use arazzo_jrpc::{Description, Engine};
    /// use serde_json::{json, Value};
    ///
    /// /// A control port that echoes the call's payload.
    /// struct Echo;
    ///
    /// impl Invoker for Echo {
    ///     fn invoke(&mut self, call: &Call) -> Result<Value, CallFailure> {
    ///         Ok(json!({"objno": 1, "echo": call.payload}))
    ///     }
    /// }
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows:
    ///   - workflowId: mark
    ///     inputs: { type: object, properties: { number: { type: string } } }
    ///     outputs: { objno: $steps.mark.outputs.objno }
    ///     steps:
    ///       - stepId: mark
    ///         operationId: $sourceDescriptions.pu.target.mark
    ///         requestBody: { payload: { number: $inputs.number } }
    ///         outputs: { objno: $response.body#/objno, sent: $response.body#/echo/number }
    /// "#)?;
    /// let mut port = Echo;
    /// let out = Engine::new(&d).with_invoker("pu", &mut port).run("mark", json!({"number": "7926"}))?;
    /// assert_eq!(out.outputs["objno"], 1);
    /// assert_eq!(out.steps[0].outputs["sent"], "7926");
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_invoker(mut self, source: &str, invoker: &'a mut dyn Invoker) -> Self {
        self.invokers.push((source.to_string(), invoker));
        self
    }

    /// Replaces `std::thread::sleep` for `retryAfter` and `Retry-After`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use arazzo_jrpc::transport::{Call, CallFailure, Invoker};
    /// use arazzo_jrpc::{Description, Engine};
    /// use serde_json::{json, Value};
    ///
    /// /// Fails the first call, answers the second.
    /// # #[derive(Default)]
    /// struct Flaky { calls: u32 }
    ///
    /// impl Invoker for Flaky {
    ///     fn invoke(&mut self, _: &Call) -> Result<Value, CallFailure> {
    ///         self.calls += 1;
    ///         if self.calls == 1 { Err(CallFailure::new("busy", json!(null))) } else { Ok(json!({})) }
    ///     }
    /// }
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows:
    ///   - workflowId: w
    ///     steps:
    ///       - stepId: s
    ///         operationId: $sourceDescriptions.pu.op
    ///         onFailure: [ { name: again, type: retry, retryAfter: 0.5, retryLimit: 3 } ]
    /// "#)?;
    /// let mut port = Flaky::default();
    /// let mut slept = vec![];
    /// let mut sleep = |d: Duration| slept.push(d);
    /// Engine::new(&d).with_invoker("pu", &mut port).with_sleeper(&mut sleep).run("w", json!({}))?;
    /// assert_eq!(slept, [Duration::from_millis(500)]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_sleeper(mut self, sleeper: &'a mut dyn FnMut(Duration)) -> Self {
        self.sleeper = Some(sleeper);
        self
    }

    /// Calls `f` after every attempt, in execution order.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::{Description, Done, Engine};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows:
    ///   - workflowId: w
    ///     steps:
    ///       - { stepId: a, operationId: $sourceDescriptions.pu.op }
    ///       - { stepId: b, operationId: $sourceDescriptions.pu.op }
    /// "#)?;
    /// let mut seen = vec![];
    /// let mut on_step = |done: &Done| seen.push(done.step_id.clone());
    /// Engine::new(&d).dry_run().on_step(&mut on_step).run("w", json!({}))?;
    /// assert_eq!(seen, ["a", "b"]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn on_step(mut self, f: &'a mut dyn FnMut(&Done)) -> Self {
        self.on_step = Some(f);
        self
    }

    /// Caps the attempts of one run (a `goto` may loop forever).
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::{Description, Engine};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows:
    ///   - workflowId: w
    ///     steps:
    ///       - { stepId: a, operationId: $sourceDescriptions.pu.op, onSuccess: [ { name: again, type: goto, stepId: a } ] }
    /// "#)?;
    /// let e = Engine::new(&d).dry_run().with_step_limit(10).run("w", json!({})).unwrap_err();
    /// assert!(e.to_string().contains("10"), "{e}");
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_step_limit(mut self, limit: usize) -> Self {
        self.step_limit = limit;
        self
    }

    /// Walks the graph without calling anything: responses and outputs are
    /// `null`, parameters and payloads are still evaluated.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::{Description, Engine};
    /// use serde_json::{json, Value};
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows:
    ///   - workflowId: w
    ///     steps:
    ///       - { stepId: s, operationId: $sourceDescriptions.pu.op, outputs: { id: $response.body#/id } }
    /// "#)?;
    /// let out = Engine::new(&d).dry_run().run("w", json!({}))?;
    /// assert_eq!(out.steps[0].outputs["id"], Value::Null, "nothing was called");
    /// # Ok(())
    /// # }
    /// ```
    pub fn dry_run(mut self) -> Self {
        self.dry = true;
        self
    }

    /// Runs one workflow of the entry document in a fresh session.
    ///
    /// # Errors
    ///
    /// As for [`Session::run`]; the message names the workflow and the step, e.g.
    /// `workflow w, step a: not satisfied: $statusCode == 200`.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::{Description, Engine};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows:
    ///   - workflowId: w
    ///     inputs: { type: object, required: [ number ], properties: { number: { type: string } } }
    ///     steps:
    ///       - { stepId: s, operationId: $sourceDescriptions.pu.op }
    /// "#)?;
    /// let out = Engine::new(&d).dry_run().run("w", json!({"number": "7926"}))?;
    /// assert_eq!(out.steps.len(), 1);
    /// assert!(Engine::new(&d).dry_run().run("w", json!({})).is_err(), "number is required");
    /// # Ok(())
    /// # }
    /// ```
    pub fn run(self, workflow_id: &str, inputs: Value) -> Result<Outcome> {
        self.session().run(workflow_id, inputs)
    }

    /// Turns the engine into a session: several runs sharing `$workflows.*`
    /// state and workflow `dependsOn` prerequisites (each runs once).
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::{Call, CallFailure, Invoker};
    /// use arazzo_jrpc::{Description, Engine};
    /// use serde_json::{json, Value};
    ///
    /// # struct Echo;
    /// # impl Invoker for Echo {
    /// #     fn invoke(&mut self, call: &Call) -> Result<Value, CallFailure> { Ok(json!({"echo": call.payload})) }
    /// # }
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows:
    ///   - workflowId: login
    ///     outputs: { token: $steps.l.outputs.token }
    ///     steps:
    ///       - { stepId: l, operationId: $sourceDescriptions.pu.login, requestBody: { payload: { token: t1 } }, outputs: { token: $response.body#/echo/token } }
    ///   - workflowId: fetch
    ///     steps:
    ///       - { stepId: f, operationId: $sourceDescriptions.pu.fetch, requestBody: { payload: { token: $workflows.login.outputs.token } }, outputs: { used: $response.body#/echo/token } }
    /// "#)?;
    /// let mut port = Echo;
    /// let mut session = Engine::new(&d).with_invoker("pu", &mut port).session();
    /// session.run("login", json!({}))?;
    /// let out = session.run("fetch", json!({}))?;
    /// assert_eq!(out.steps[0].outputs["used"], "t1", "the first run's outputs are visible");
    /// # Ok(())
    /// # }
    /// ```
    pub fn session(self) -> Session<'a> {
        Session { engine: self, workflows: BTreeMap::new(), finished: BTreeSet::new(), subscribed: BTreeSet::new(), done: Vec::new(), executed: 0, aborted: false }
    }
}

/// Several runs over one engine; what one run leaves (`$workflows.*`,
/// completed prerequisites) is visible to the next.
pub struct Session<'a> {
    engine: Engine<'a>,
    /// `$workflows.<id>.*` for every workflow run so far.
    workflows: BTreeMap<String, WorkflowState>,
    /// Workflows completed in this session, by (document, workflowId).
    finished: BTreeSet<(usize, String)>,
    /// Receive channels the messaging transport is subscribed to.
    subscribed: BTreeSet<String>,
    /// Attempts of the current run.
    done: Vec<Done>,
    /// Attempts counted against the step limit in the current run.
    executed: usize,
    /// Set by an engine-level error, so that child workflow steps do not
    /// turn it into a handled step failure.
    aborted: bool,
}

impl Session<'_> {
    /// Runs one workflow of the entry document (`workflowId` or
    /// `$sourceDescriptions.<name>.<workflowId>`).
    ///
    /// # Errors
    ///
    /// Invalid inputs, an unhandled step failure, an unresolvable operation, a
    /// missing transport, the step limit, or runaway recursion.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::{Description, Engine};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows:
    ///   - workflowId: login
    ///     steps: [ { stepId: l, operationId: $sourceDescriptions.pu.login } ]
    ///   - workflowId: fetch
    ///     dependsOn: [ login ]
    ///     steps: [ { stepId: f, operationId: $sourceDescriptions.pu.fetch } ]
    /// "#)?;
    /// let mut session = Engine::new(&d).dry_run().session();
    /// let out = session.run("fetch", json!({}))?;
    /// assert_eq!(out.steps.len(), 2, "login ran first as a prerequisite");
    /// let out = session.run("fetch", json!({}))?;
    /// assert_eq!(out.steps.len(), 1, "once per session");
    /// # Ok(())
    /// # }
    /// ```
    pub fn run(&mut self, workflow_id: &str, inputs: Value) -> Result<Outcome> {
        self.done.clear();
        self.executed = 0;
        self.aborted = false;
        let desc = self.engine.desc;
        let (doc, w) = desc.workflow(0, workflow_id).ok_or_else(|| Error(format!("workflow {workflow_id} not found")))?;
        self.subscribe_receives()?;
        let outputs = self.run_workflow(doc, w, inputs, 0)?;
        Ok(Outcome { outputs, steps: std::mem::take(&mut self.done) })
    }

    /// Subscribes the messaging transport to the channel of every receive
    /// step in the description — before the first step, once per session —
    /// so a message published while earlier steps run waits for its receive
    /// (a sequential run has no receive in flight). Nothing in a dry run or
    /// without messaging.
    fn subscribe_receives(&mut self) -> Result<()> {
        if self.engine.dry || self.engine.messaging.is_none() {
            return Ok(());
        }
        let desc = self.engine.desc;
        let mut channels = BTreeSet::new();
        for (d, loaded) in desc.documents().iter().enumerate() {
            for s in loaded.doc.workflows.iter().flat_map(|w| &w.steps) {
                if let Ok(step::Target::Message { action: AsyncAction::Receive, channel }) = self.resolve(d, s) {
                    channels.insert(channel.address);
                }
            }
        }
        channels.retain(|c| !self.subscribed.contains(c));
        for c in channels {
            let bus = self.engine.messaging.as_deref_mut().expect("checked above");
            if let Err(e) = bus.subscribe(&c) {
                return Err(self.fatal(format!("subscribe to {c}: {e}")));
            }
            self.subscribed.insert(c);
        }
        Ok(())
    }

    /// An engine-level error: marks the run aborted and returns it.
    fn fatal(&mut self, message: String) -> Error {
        self.aborted = true;
        Error(message)
    }

    /// Records an attempt and reports it to the caller.
    fn emit(&mut self, done: Done) {
        if let Some(f) = self.engine.on_step.as_mut() {
            f(&done);
        }
        self.done.push(done);
    }

    /// Waits `d` via the caller's sleeper; nothing in a dry run or for zero.
    fn sleep(&mut self, d: Duration) {
        if self.engine.dry || d.is_zero() {
            return;
        }
        match self.engine.sleeper.as_mut() {
            Some(f) => f(d),
            None => std::thread::sleep(d),
        }
    }
}
