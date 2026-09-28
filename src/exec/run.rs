//! One workflow run: workflow `dependsOn`, inputs, the step walk, outputs.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::actions::Flow;
use super::step::Attempt;
use super::{Done, Session};
use crate::description::operation_name;
use crate::document::SOURCE_PREFIX;
use crate::error::{Error, Result};
use crate::expr::{Scope, WorkflowState};
use crate::model::{Action, ActionKind, Step, StepTarget, ValueOrSelector, Workflow};
use crate::{inputs, payload};

/// Nesting of workflow steps, gotos and retries into workflows.
const MAX_DEPTH: usize = 32;

/// A workflow being run: where it lives and what its steps produced.
pub(super) struct Frame<'a> {
    /// Index of its document in the description.
    pub doc: usize,
    pub workflow: &'a Workflow,
    /// Document scope plus `$inputs` and `$steps` of this run.
    pub scope: Scope,
    pub depth: usize,
    /// Whether the run ended through a failure action (`end` after a failure).
    pub ended_failed: bool,
}

impl Frame<'_> {
    /// Names a step of this workflow for error messages.
    pub(super) fn at(&self, step: &Step) -> String {
        format!("workflow {}, step {}", self.workflow.workflow_id, step.step_id)
    }
}

impl<'a> Session<'a> {
    /// Runs workflow `w` of document `doc` with the given inputs.
    ///
    /// Fails on invalid inputs, a failed prerequisite or an unhandled step failure.
    pub(super) fn run_workflow(&mut self, doc: usize, w: &'a Workflow, given: Value, depth: usize) -> Result<Map<String, Value>> {
        let id = &w.workflow_id;
        if depth > MAX_DEPTH {
            return Err(self.fatal(format!("workflow {id}: nesting depth exceeds {MAX_DEPTH}, recursive workflow?")));
        }
        self.check_targets(doc, w)?;
        self.run_prerequisites(doc, w, depth)?;
        let mut frame = self.start(doc, w, given, depth)?;
        self.walk(&mut frame)?;
        let outputs = self.workflow_outputs(&frame)?;
        self.workflows.entry(id.clone()).or_default().outputs = outputs.clone();
        self.finished.insert((doc, id.clone()));
        Ok(outputs)
    }

    /// Runs `step` alone for `retry` + `stepId`: its call, criteria and
    /// outputs, not its actions.
    pub(super) fn side_step(&mut self, frame: &mut Frame<'a>, step: &'a Step) -> Result<()> {
        let at = frame.at(step);
        self.tick(&at)?;
        let attempt = self.attempt(frame, step)?;
        self.emit(report(frame, step, &attempt, None));
        match attempt.failure {
            Some(why) => Err(Error(format!("{at}: {why}"))),
            None => {
                self.record(frame, step, &attempt.outputs);
                Ok(())
            }
        }
    }

    /// The scope a step of this frame evaluates in: the frame's plus `$workflows.*`.
    pub(super) fn scope_of(&self, frame: &Frame) -> Scope {
        Scope { workflows: self.workflows.clone(), ..frame.scope.clone() }
    }

    /// Evaluates a set of outputs; each is `null` in a dry run.
    ///
    /// Outputs that are unset inputs are left out.
    pub(super) fn outputs(&self, list: &BTreeMap<String, ValueOrSelector>, scope: &Scope) -> Result<Map<String, Value>> {
        let mut out = Map::new();
        for (name, v) in list {
            if self.engine.dry {
                out.insert(name.clone(), Value::Null);
            } else if let Some(v) = payload::value(v, scope).map_err(|e| Error(format!("output {name}: {e}")))? {
                out.insert(name.clone(), v);
            }
        }
        Ok(out)
    }

    /// Resolves every step's target before anything runs, so a bad target stops the run up front.
    fn check_targets(&mut self, doc: usize, w: &'a Workflow) -> Result<()> {
        for s in &w.steps {
            self.resolve(doc, s).map_err(|e| self.fatal(format!("workflow {}, step {}: {e}", w.workflow_id, s.step_id)))?;
        }
        Ok(())
    }

    /// Workflow `dependsOn`: runs each prerequisite not yet finished in this session.
    fn run_prerequisites(&mut self, doc: usize, w: &Workflow, depth: usize) -> Result<()> {
        for dep in &w.depends_on {
            let (k, pre) = self.engine.desc.workflow(doc, dep).ok_or_else(|| Error(format!("workflow {}: dependsOn {dep}: not found", w.workflow_id)))?;
            if !self.finished.contains(&(k, pre.workflow_id.clone())) {
                self.run_workflow(k, pre, Value::Object(Map::new()), depth + 1)
                    .map_err(|e| Error(format!("workflow {}: dependsOn {dep}: {e}", w.workflow_id)))?;
            }
        }
        Ok(())
    }

    /// Resolves the inputs, opens `$workflows.<id>` and builds the frame the walk runs in.
    fn start(&mut self, doc: usize, w: &'a Workflow, given: Value, depth: usize) -> Result<Frame<'a>> {
        let desc = self.engine.desc;
        let components = desc.documents()[doc].doc.components.as_ref();
        let inputs = inputs::resolve(w.inputs.as_ref(), components, &given).map_err(|e| Error(format!("workflow {}: inputs: {e}", w.workflow_id)))?;
        self.workflows.insert(w.workflow_id.clone(), WorkflowState { inputs: inputs.clone(), ..Default::default() });
        let scope = Scope { inputs: Value::Object(inputs), ..Scope::for_document(desc, doc) };
        Ok(Frame { doc, workflow: w, scope, depth, ended_failed: false })
    }

    /// Steps in order, jumping where actions say.
    fn walk(&mut self, frame: &mut Frame<'a>) -> Result<()> {
        let steps = &frame.workflow.steps;
        let mut i = 0;
        // How many times each retry fired on the current step (retryLimit).
        let mut retries: BTreeMap<String, u32> = BTreeMap::new();
        while let Some(step) = steps.get(i) {
            let flow = self.step_once(frame, step, &mut retries)?;
            if !matches!(flow, Flow::Retry) {
                retries.clear();
            }
            match flow {
                Flow::Next => i += 1,
                Flow::Goto(j) => i = j,
                Flow::Retry => {}
                Flow::End => return Ok(()),
            }
        }
        Ok(())
    }

    /// One attempt of `step` plus the action it triggers.
    ///
    /// `Next` on a success no action handles, otherwise what the action says. A
    /// failure no action handles is an error.
    fn step_once(&mut self, frame: &mut Frame<'a>, step: &'a Step, retries: &mut BTreeMap<String, u32>) -> Result<Flow> {
        let at = frame.at(step);
        self.tick(&at)?;
        self.check_dependencies(frame, step).map_err(|e| Error(format!("{at}: {e}")))?;
        let attempt = self.attempt(frame, step)?;
        if attempt.failure.is_none() {
            self.record(frame, step, &attempt.outputs);
        }
        let chosen = self.pick(frame, step, &attempt, retries)?;
        self.emit(report(frame, step, &attempt, chosen.as_ref()));
        match (chosen, &attempt.failure) {
            (Some(a), failure) => {
                let flow = self.perform(frame, &a, &attempt.scope, retries)?;
                frame.ended_failed = matches!(flow, Flow::End) && failure.is_some();
                Ok(flow)
            }
            (None, None) => Ok(Flow::Next),
            (None, Some(why)) => Err(Error(format!("{at}: {why}"))),
        }
    }

    /// Counts an attempt against the step limit.
    fn tick(&mut self, at: &str) -> Result<()> {
        self.executed += 1;
        let limit = self.engine.step_limit;
        if self.executed > limit {
            return Err(self.fatal(format!("{at}: more than {limit} steps in one run, loop in onSuccess/onFailure?")));
        }
        Ok(())
    }

    /// Step `dependsOn`: a plain stepId must have completed in this run,
    /// `$workflows.X.steps.Y` earlier in the session. Nothing is triggered.
    fn check_dependencies(&self, frame: &Frame, step: &Step) -> std::result::Result<(), String> {
        for dep in &step.depends_on {
            let done = match dep.rsplit_once(".steps.") {
                Some((wf, st)) if dep.starts_with('$') => {
                    let id = operation_name(wf.strip_prefix("$workflows.").unwrap_or(wf));
                    self.workflows.get(id).is_some_and(|w| w.steps.contains_key(st))
                }
                _ => frame.scope.steps.contains_key(dep),
            };
            if !done {
                return Err(format!("dependency {dep} has not run yet (dependsOn does not start steps)"));
            }
        }
        Ok(())
    }

    /// Remembers a completed step's outputs for `$steps.*` and `$workflows.*.steps.*`.
    fn record(&mut self, frame: &mut Frame, step: &Step, outputs: &Map<String, Value>) {
        frame.scope.steps.insert(step.step_id.clone(), outputs.clone());
        let state = self.workflows.entry(frame.workflow.workflow_id.clone()).or_default();
        state.steps.insert(step.step_id.clone(), outputs.clone());
    }

    /// Workflow outputs; after an `end` on failure unevaluable ones are `null`.
    fn workflow_outputs(&self, frame: &Frame) -> Result<Map<String, Value>> {
        let scope = self.scope_of(frame);
        let w = frame.workflow;
        if !frame.ended_failed {
            return self.outputs(&w.outputs, &scope).map_err(|e| Error(format!("workflow {}: {e}", w.workflow_id)));
        }
        Ok(w.outputs.iter().map(|(n, v)| (n.clone(), payload::value(v, &scope).ok().flatten().unwrap_or_default())).collect())
    }
}

/// The report of one attempt, with the action it fired (if any).
fn report(frame: &Frame, step: &Step, attempt: &Attempt, action: Option<&Action>) -> Done {
    let operation = match &step.target {
        StepTarget::OperationId(s) | StepTarget::WorkflowId(s) => s.strip_prefix(SOURCE_PREFIX).unwrap_or(s),
        StepTarget::OperationPath(s) | StepTarget::ChannelPath(s) => s,
    };
    Done {
        workflow_id: frame.workflow.workflow_id.clone(),
        step_id: step.step_id.clone(),
        operation: operation.to_string(),
        outputs: attempt.outputs.clone(),
        action: action.map(|a| a.name.clone()),
        failed: attempt.failure.clone(),
        retrying: action.is_some_and(|a| a.kind == ActionKind::Retry),
    }
}
