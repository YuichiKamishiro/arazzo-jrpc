//! Success and Failure Actions (spec 5.8.7, 5.8.8): which one fires and
//! what it does to the walk.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Map, Value};

use super::run::Frame;
use super::step::Attempt;
use super::Session;
use crate::error::{Error, Result};
use crate::expr::Scope;
use crate::model::{Action, ActionKind, Step};
use crate::transport::header;
use crate::{condition, payload, refs};

/// Where the walk goes after a step.
pub(super) enum Flow {
    Next,
    Goto(usize),
    /// Same step again.
    Retry,
    End,
}

impl<'a> Session<'a> {
    /// The action this attempt triggers: the first of the effective list
    /// (step's own, then inherited workflow ones) whose criteria hold and,
    /// for `retry`, whose `retryLimit` (default 1) is not used up.
    ///
    /// `None` when no action applies. Fails on an unresolvable `$components` reference.
    pub(super) fn pick(&self, frame: &Frame, step: &Step, attempt: &Attempt, retries: &BTreeMap<String, u32>) -> Result<Option<Action>> {
        let doc = &self.engine.desc.documents()[frame.doc].doc;
        let w = frame.workflow;
        let success = attempt.failure.is_none();
        let (own, inherited) = if success { (&step.on_success, &w.success_actions) } else { (&step.on_failure, &w.failure_actions) };
        let actions = refs::step_actions(doc, inherited, own, success)?;
        Ok(actions.into_iter().find(|a| {
            let spent = a.kind == ActionKind::Retry && retries.get(&a.name).copied().unwrap_or(0) >= a.retry_limit.unwrap_or(1);
            !spent && condition::all(&a.criteria, &attempt.scope)
        }))
    }

    /// Carries out an action picked for the current step.
    ///
    /// `End` for `end` and for a `goto` to a workflow (which has already run),
    /// `Goto(i)` for a step, `Retry` after the delay and after the optional step or
    /// workflow ran.
    pub(super) fn perform(&mut self, frame: &mut Frame<'a>, a: &Action, scope: &Scope, retries: &mut BTreeMap<String, u32>) -> Result<Flow> {
        let w = frame.workflow;
        let target_step = |id: &str| w.step_index(id).ok_or_else(|| Error(format!("action {}: no step {id}", a.name)));
        match a.kind {
            ActionKind::End => Ok(Flow::End),
            ActionKind::Goto => match &a.step_id {
                Some(id) => Ok(Flow::Goto(target_step(id)?)),
                None => {
                    self.jump(frame, a, scope)?;
                    Ok(Flow::End)
                }
            },
            ActionKind::Retry => {
                *retries.entry(a.name.clone()).or_default() += 1;
                self.sleep(retry_delay(a, scope));
                if let Some(id) = &a.step_id {
                    let i = target_step(id)?;
                    self.side_step(frame, &w.steps[i])?;
                } else if a.workflow_id.is_some() {
                    self.jump(frame, a, scope)?;
                }
                Ok(Flow::Retry)
            }
        }
    }

    /// Runs the action's `workflowId` with its parameters as inputs.
    fn jump(&mut self, frame: &Frame, a: &Action, scope: &Scope) -> Result<()> {
        let desc = self.engine.desc;
        let reference = a.workflow_id.as_deref().ok_or_else(|| Error(format!("action {}: neither stepId nor workflowId", a.name)))?;
        let (k, target) = desc.workflow(frame.doc, reference).ok_or_else(|| Error(format!("action {}: workflow {reference} not found", a.name)))?;
        let doc = &desc.documents()[frame.doc].doc;
        let mut inputs = Map::new();
        for p in &a.parameters {
            let p = refs::parameter(doc, p)?;
            if let Some(v) = payload::value(&p.value, scope).map_err(|e| Error(format!("action {}: parameter {}: {e}", a.name, p.name)))? {
                inputs.insert(p.name, v);
            }
        }
        self.run_workflow(k, target, Value::Object(inputs), frame.depth + 1).map(|_| ())
    }
}

/// How long to wait before a retry: the response's `Retry-After` seconds
/// win over `retryAfter` (spec 5.8.8.1).
///
/// Zero when neither is set or the value is not a positive number.
fn retry_delay(a: &Action, scope: &Scope) -> Duration {
    let from_header = scope.exchange.as_ref().and_then(|x| header(&x.response_headers, "Retry-After")).and_then(|v| v.trim().parse::<f64>().ok());
    from_header.or(a.retry_after).filter(|s| s.is_finite() && *s > 0.0).map(Duration::from_secs_f64).unwrap_or_default()
}
