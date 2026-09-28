//! One attempt of one step: resolve what it targets, evaluate parameters
//! and payload, call the transport, check successCriteria, read outputs.

use std::time::Duration;

use serde_json::{Map, Value};

use super::run::Frame;
use super::Session;
use crate::asyncapi::{self, Channel};
use crate::description::{operation_name, Description, Target as Source};
use crate::document::split_source_path;
use crate::error::Result;
use crate::expr::{embed, interpolate, Scope};
use crate::model::{AsyncAction, ParameterIn, SourceDescription, Step, StepTarget, Workflow};
use crate::openapi::{self, Operation, ParamValue};
use crate::transport::{Call, CallFailure, Message, MessageTransport};
use crate::{condition, payload, refs};

/// Receive steps without `timeout` wait this long.
const DEFAULT_RECEIVE_TIMEOUT: Duration = Duration::from_secs(30);

/// What one attempt produced.
pub(super) struct Attempt {
    /// The step's scope with the exchange, message or body it got.
    pub scope: Scope,
    /// Evaluated outputs; empty when the attempt failed.
    pub outputs: Map<String, Value>,
    /// Why the attempt failed, when it did.
    pub failure: Option<String>,
}

/// What a step invokes, resolved against the description.
pub(super) enum Target<'a> {
    Http(Operation<'a>),
    Message { action: AsyncAction, channel: Channel },
    Opaque { source: &'a SourceDescription, operation: String },
    Workflow(usize, &'a Workflow),
}

impl<'a> Session<'a> {
    /// Resolves the step's target and checks that a transport serves it.
    pub(super) fn resolve(&self, doc: usize, step: &'a Step) -> std::result::Result<Target<'a>, String> {
        let desc: &'a Description = self.engine.desc;
        match &step.target {
            StepTarget::WorkflowId(r) => desc.workflow(doc, r).map(|(k, w)| Target::Workflow(k, w)).ok_or(format!("workflow {r} not found")),
            _ => {
                let name = source_name(desc, doc, step)?;
                let target = self.resolve_in_source(desc, doc, step, name)?;
                self.check_transport(&target, name)?;
                Ok(target)
            }
        }
    }

    /// Runs one attempt of `step`.
    ///
    /// A failure the workflow may handle (transport error, unmet criterion, failed
    /// child workflow) is in `failure`; `Err` is an engine-level stop (unresolvable
    /// target, step limit, recursion).
    pub(super) fn attempt(&mut self, frame: &Frame<'a>, step: &'a Step) -> Result<Attempt> {
        let target = self.resolve(frame.doc, step).map_err(|e| self.fatal(format!("{}: {e}", frame.at(step))))?;
        let mut scope = self.scope_of(frame);
        let failure = match self.prepare(frame, step, &scope) {
            Err(why) => Some(why),
            Ok((params, body)) => self.invoke(frame, step, target, params, body, &mut scope)?,
        };
        let failure = match failure {
            None if !self.engine.dry => condition::first_failure(&step.success_criteria, &scope),
            other => other,
        };
        let (outputs, failure) = match failure {
            Some(why) => (Map::new(), Some(why)),
            None => match self.outputs(&step.outputs, &scope) {
                Ok(o) => (o, None),
                Err(e) => (Map::new(), Some(e.0)),
            },
        };
        Ok(Attempt { scope, outputs, failure })
    }

    /// The operation or channel a step names inside source `name`.
    fn resolve_in_source(&self, desc: &'a Description, doc: usize, step: &'a Step, name: &str) -> std::result::Result<Target<'a>, String> {
        let resolved = desc.source(doc, name).ok_or(format!("source {name} is not declared in sourceDescriptions"))?;
        let missing = |what: &str| format!("{what} not found in {name}");
        match (&step.target, &resolved.target) {
            (StepTarget::OperationId(id), Source::OpenApi(api)) => {
                openapi::find_operation(api, operation_name(id)).map(Target::Http).ok_or_else(|| missing(&format!("operation {}", operation_name(id))))
            }
            (StepTarget::OperationPath(p), Source::OpenApi(api)) => {
                let (_, pointer) = split_source_path(p).ok_or(format!("operationPath {p}: expected {{$sourceDescriptions.<name>.url}}#/<pointer>"))?;
                openapi::at_pointer(api, pointer).map(Target::Http).ok_or_else(|| missing(&format!("operation {pointer}")))
            }
            (StepTarget::OperationId(id), Source::AsyncApi(api)) => {
                let op = asyncapi::find_operation(api, operation_name(id)).ok_or_else(|| missing(&format!("operation {}", operation_name(id))))?;
                Ok(Target::Message { action: step.action.unwrap_or(op.action), channel: op.channel })
            }
            (StepTarget::ChannelPath(p), Source::AsyncApi(api)) => {
                let reference = split_source_path(p).map(|(_, ptr)| ptr).unwrap_or(p);
                let channel = asyncapi::find_channel(api, reference).ok_or_else(|| missing(&format!("channel {reference}")))?;
                let action = step.action.ok_or("channelPath requires action (send|receive)")?;
                Ok(Target::Message { action, channel })
            }
            (StepTarget::OperationId(id), Source::Opaque) => {
                let source = desc.documents()[doc].doc.source(name).ok_or(format!("source {name} is not declared"))?;
                Ok(Target::Opaque { source, operation: operation_name(id).to_string() })
            }
            (_, Source::Missing) => Err(format!("source {name}: document {} not loaded", resolved.uri)),
            _ => Err(format!("step does not match source {name}")),
        }
    }

    /// A transport for the target must be registered (except in a dry run).
    fn check_transport(&self, target: &Target, source: &str) -> std::result::Result<(), String> {
        let e = &self.engine;
        let missing = match target {
            Target::Http(_) if e.http.is_none() => "no HTTP transport (Engine::with_http)",
            Target::Message { .. } if e.messaging.is_none() => "no messaging transport (Engine::with_messaging)",
            Target::Opaque { .. } if !e.invokers.iter().any(|(n, _)| n == source) => "no Invoker (Engine::with_invoker)",
            _ => return Ok(()),
        };
        if e.dry {
            return Ok(());
        }
        Err(format!("source {source}: {missing}"))
    }

    /// Evaluated parameters (workflow-level merged in) and payload.
    fn prepare(&self, frame: &Frame, step: &Step, scope: &Scope) -> std::result::Result<(Vec<ParamValue>, Option<Value>), String> {
        let doc = &self.engine.desc.documents()[frame.doc].doc;
        let mut params = Vec::new();
        for p in refs::step_parameters(doc, &frame.workflow.parameters, &step.parameters).map_err(|e| e.0)? {
            if let Some(v) = payload::value(&p.value, scope).map_err(|e| format!("parameter {}: {e}", p.name))? {
                params.push((p.name, p.location, v));
            }
        }
        let body = match &step.request_body {
            Some(b) => payload::request_body(b, scope).map_err(|e| format!("requestBody: {e}"))?,
            None => None,
        };
        Ok((params, body))
    }

    /// Calls the target; in a dry run only workflow steps run (dry as well).
    ///
    /// `Ok(None)` on success (the answer is in `scope`), `Ok(Some(why))` for a step
    /// failure; `Err` only for a child workflow's engine-level stop.
    fn invoke(&mut self, frame: &Frame, step: &Step, target: Target<'a>, params: Vec<ParamValue>, body: Option<Value>, scope: &mut Scope) -> Result<Option<String>> {
        if self.engine.dry && !matches!(target, Target::Workflow(..)) {
            scope.body = Some(Value::Null);
            return Ok(None);
        }
        Ok(match target {
            Target::Http(op) => self.call_http(&op, step, &params, body.as_ref(), scope),
            Target::Message { action, channel } => self.call_message(action, &channel, step, &params, body, scope),
            Target::Opaque { source, operation } => self.call_invoker(source, operation, step, params, body, scope),
            Target::Workflow(k, w) => return self.call_workflow(frame, k, w, params, scope),
        })
    }

    /// Sends the HTTP request; a transport error is a step failure.
    ///
    /// `None` on success, with `scope.exchange` set.
    fn call_http(&mut self, op: &Operation, step: &Step, params: &[ParamValue], body: Option<&Value>, scope: &mut Scope) -> Option<String> {
        let content_type = step.request_body.as_ref().and_then(|b| b.content_type.as_deref());
        let (request, exchange) = openapi::build_request(op, params, body, content_type, step.timeout.map(Duration::from_millis));
        let http = self.engine.http.as_mut().expect("checked in resolve");
        match http.send(&request) {
            Ok(r) => {
                scope.exchange = Some(openapi::read_response(exchange, &r));
                None
            }
            Err(e) => Some(format!("{} {}: {e}", request.method, request.url)),
        }
    }

    /// Publishes or waits for a message on the channel, `in: channel`
    /// parameters substituted into its address.
    ///
    /// `None` on success, with `scope.message` set.
    fn call_message(&mut self, action: AsyncAction, channel: &Channel, step: &Step, params: &[ParamValue], body: Option<Value>, scope: &mut Scope) -> Option<String> {
        let address = channel_address(channel, params);
        let bus = self.engine.messaging.as_deref_mut().expect("checked in resolve");
        match action {
            AsyncAction::Send => send_message(bus, address, params, body, scope),
            AsyncAction::Receive => receive_message(bus, &address, step, scope),
        }
    }

    /// Calls the caller's `Invoker` registered for the opaque source.
    ///
    /// `None` on success; on a `CallFailure` its body still goes to `scope.body`.
    fn call_invoker(&mut self, source: &SourceDescription, operation: String, step: &Step, params: Vec<ParamValue>, body: Option<Value>, scope: &mut Scope) -> Option<String> {
        let parameters = params.into_iter().map(|(n, l, v)| (n, l.map(location_name), v)).collect();
        let call = Call { source, operation, parameters, payload: body, timeout: step.timeout.map(Duration::from_millis) };
        let invoker = self.engine.invokers.iter_mut().find(|(n, _)| *n == source.name).map(|(_, i)| i).expect("checked in resolve");
        answer(invoker.invoke(&call), scope)
    }

    /// Runs a child workflow; its outputs become `$outputs` for this step.
    ///
    /// `Ok(None)` on success, `Ok(Some(error))` for a failure the parent may handle,
    /// `Err` for an engine-level stop in the child.
    fn call_workflow(&mut self, frame: &Frame, k: usize, w: &'a Workflow, params: Vec<ParamValue>, scope: &mut Scope) -> Result<Option<String>> {
        let inputs: Map<String, Value> = params.into_iter().filter(|(_, l, _)| l.is_none()).map(|(n, _, v)| (n, v)).collect();
        match self.run_workflow(k, w, Value::Object(inputs), frame.depth + 1) {
            Ok(outputs) => {
                scope.outputs = outputs;
                scope.workflows = self.workflows.clone();
                Ok(None)
            }
            Err(e) if self.aborted => Err(e),
            Err(e) => Ok(Some(e.0)),
        }
    }
}

/// The source an operation step belongs to.
///
/// Fails when an unqualified `operationId` is ambiguous between several sources.
fn source_name<'a>(desc: &'a Description, doc: usize, step: &'a Step) -> std::result::Result<&'a str, String> {
    desc.documents()[doc].doc.step_source(step).ok_or_else(|| match &step.target {
        StepTarget::OperationId(id) => format!("operationId {id}: several sources declare it, write $sourceDescriptions.<name>.{id}"),
        _ => "cannot determine the step's source".to_string(),
    })
}

/// The channel address with `in: channel` parameters substituted.
fn channel_address(channel: &Channel, params: &[ParamValue]) -> String {
    let mut address = channel.address.clone();
    for (name, _, v) in params.iter().filter(|(_, l, _)| *l == Some(ParameterIn::Channel)) {
        address = address.replace(&format!("{{{name}}}"), &embed(v));
    }
    address
}

/// Publishes the payload with `in: header` parameters as headers; the message goes to `scope.message`.
fn send_message(bus: &mut dyn MessageTransport, channel: String, params: &[ParamValue], body: Option<Value>, scope: &mut Scope) -> Option<String> {
    let headers = params.iter().filter(|(_, l, _)| *l == Some(ParameterIn::Header)).map(|(n, _, v)| (n.clone(), embed(v))).collect();
    let message = Message { channel, headers, payload: body.unwrap_or_default() };
    let failure = bus.send(&message).err().map(|e| format!("send to {}: {e}", message.channel));
    scope.message = Some(message);
    failure
}

/// Waits for a message on `address` matching the step's `correlationId`, up to its `timeout` (30 s by default).
fn receive_message(bus: &mut dyn MessageTransport, address: &str, step: &Step, scope: &mut Scope) -> Option<String> {
    let correlation = match correlation_id(step, scope) {
        Ok(c) => c,
        Err(why) => return Some(why),
    };
    let timeout = step.timeout.map(Duration::from_millis).unwrap_or(DEFAULT_RECEIVE_TIMEOUT);
    match bus.receive(address, correlation.as_deref(), timeout) {
        Ok(Some(m)) => {
            scope.message = Some(m);
            None
        }
        Ok(None) => Some(format!("step {}: no message on {address} within {} ms", step.step_id, timeout.as_millis())),
        Err(e) => Some(format!("receive from {address}: {e}")),
    }
}

/// The step's `correlationId`: a string is interpolated, any other value embedded as text.
fn correlation_id(step: &Step, scope: &Scope) -> std::result::Result<Option<String>, String> {
    Ok(match &step.correlation_id {
        None => None,
        Some(Value::String(s)) => Some(embed(&interpolate(s, scope).map_err(|e| format!("correlationId: {e}"))?)),
        Some(other) => Some(embed(other)),
    })
}

/// An invoker's answer into the scope: the body either way.
fn answer(result: std::result::Result<Value, CallFailure>, scope: &mut Scope) -> Option<String> {
    match result {
        Ok(body) => {
            scope.body = Some(body);
            None
        }
        Err(f) => {
            scope.body = Some(f.body);
            Some(f.message)
        }
    }
}

/// `in` as the spec spells it.
fn location_name(l: ParameterIn) -> String {
    serde_json::to_value(l).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}
