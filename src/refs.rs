//! Reusable Objects and workflow-level defaults, resolved into the concrete
//! parameters and actions a step runs with.
//!
//! Spec rules implemented here: a Reusable Object points into the
//! `components` of *its own* document (5.8.9) and may override a
//! parameter's `value` (5.8.10); workflow-level parameters and actions
//! apply to every step, a step entry with the same identity overrides one
//! but cannot remove the rest (5.8.4).

use crate::error::{Error, Result};
use crate::model::{Action, ActionOrRef, Document, Parameter, ParameterOrRef, ValueOrSelector};

/// Resolves a parameter entry into a concrete `Parameter`.
///
/// A referenced component gets `value` replaced when the reference sets it.
///
/// # Errors
///
/// The reference points to nothing in this document's `components`.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::model::ValueOrSelector;
/// use arazzo_jrpc::{refs, Document};
/// use serde_json::json;
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let doc = Document::parse(r#"
/// arazzo: 1.1.0
/// info: { title: t, version: '1' }
/// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
/// workflows:
///   - workflowId: w
///     steps:
///       - stepId: s
///         operationId: op
///         parameters: [ { reference: $components.parameters.page, value: 2 } ]
/// components: { parameters: { page: { name: page, in: query, value: 1 } } }
/// "#)?;
/// let p = refs::parameter(&doc, &doc.workflows[0].steps[0].parameters[0])?;
/// assert_eq!(p.name, "page");
/// assert!(matches!(p.value, ValueOrSelector::Value(ref v) if v == &json!(2)), "the reference overrides value");
/// # Ok(())
/// # }
/// ```
pub fn parameter(doc: &Document, p: &ParameterOrRef) -> Result<Parameter> {
    match p {
        ParameterOrRef::Parameter(p) => Ok(p.clone()),
        ParameterOrRef::Reference(r) => {
            let name = r.reference.strip_prefix("$components.parameters.").ok_or_else(|| {
                Error(format!("{}: a parameter must reference $components.parameters.<name>", r.reference))
            })?;
            let mut found = doc
                .components
                .as_ref()
                .and_then(|c| c.parameters.get(name))
                .cloned()
                .ok_or_else(|| Error(format!("{}: not found in this document's components", r.reference)))?;
            if let Some(v) = &r.value {
                found.value = ValueOrSelector::Value(v.clone());
            }
            Ok(found)
        }
    }
}

/// Resolves an action entry into a concrete `Action`.
///
/// # Errors
///
/// The reference points to nothing in `components.successActions` /
/// `components.failureActions`.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::model::ActionKind;
/// use arazzo_jrpc::{refs, Document};
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let doc = Document::parse(r#"
/// arazzo: 1.1.0
/// info: { title: t, version: '1' }
/// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
/// workflows:
///   - workflowId: w
///     steps:
///       - stepId: s
///         operationId: op
///         onFailure: [ { reference: $components.failureActions.again } ]
/// components: { failureActions: { again: { name: again, type: retry, retryLimit: 3 } } }
/// "#)?;
/// let a = refs::action(&doc, &doc.workflows[0].steps[0].on_failure[0], false)?;
/// assert_eq!((a.kind, a.retry_limit), (ActionKind::Retry, Some(3)));
/// assert!(refs::action(&doc, &doc.workflows[0].steps[0].on_failure[0], true).is_err(), "not a success action");
/// # Ok(())
/// # }
/// ```
pub fn action(doc: &Document, a: &ActionOrRef, success: bool) -> Result<Action> {
    match a {
        ActionOrRef::Action(a) => Ok(a.clone()),
        ActionOrRef::Reference(r) => {
            let kind = if success { "successActions" } else { "failureActions" };
            let prefix = format!("$components.{kind}.");
            let name = r
                .reference
                .strip_prefix(prefix.as_str())
                .ok_or_else(|| Error(format!("{}: expected {prefix}<name>", r.reference)))?;
            doc.components
                .as_ref()
                .and_then(|c| if success { &c.success_actions } else { &c.failure_actions }.get(name))
                .cloned()
                .ok_or_else(|| Error(format!("{}: not in this document's components.{kind}", r.reference)))
        }
    }
}

/// Merges the parameters a step runs with: the workflow's, overridden by
/// the step's entries with the same `name` + `in`.
///
/// Workflow parameters come first (overridden ones in place), then the step's
/// new ones.
///
/// # Errors
///
/// As for [`parameter`].
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::{refs, Document};
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let doc = Document::parse(r#"
/// arazzo: 1.1.0
/// info: { title: t, version: '1' }
/// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
/// workflows:
///   - workflowId: w
///     parameters:
///       - { name: X-Trace, in: header, value: wf }
///       - { name: status, in: query, value: wf }
///     steps:
///       - { stepId: s, operationId: op, parameters: [ { name: status, in: query, value: step } ] }
/// "#)?;
/// let w = &doc.workflows[0];
/// let merged = refs::step_parameters(&doc, &w.parameters, &w.steps[0].parameters)?;
/// let names: Vec<&str> = merged.iter().map(|p| p.name.as_str()).collect();
/// assert_eq!(names, ["X-Trace", "status"]);
/// # Ok(())
/// # }
/// ```
pub fn step_parameters(doc: &Document, workflow: &[ParameterOrRef], step: &[ParameterOrRef]) -> Result<Vec<Parameter>> {
    let mut out: Vec<Parameter> = workflow.iter().map(|p| parameter(doc, p)).collect::<Result<_>>()?;
    for p in step {
        let p = parameter(doc, p)?;
        match out.iter_mut().find(|q| q.name == p.name && q.location == p.location) {
            Some(slot) => *slot = p,
            None => out.push(p),
        }
    }
    Ok(out)
}

/// Merges the actions a step reacts with: the step's own, then the
/// workflow's whose names the step does not reuse.
///
/// Evaluation order: the first match wins.
///
/// # Errors
///
/// As for [`action`].
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::model::ActionKind;
/// use arazzo_jrpc::{refs, Document};
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let doc = Document::parse(r#"
/// arazzo: 1.1.0
/// info: { title: t, version: '1' }
/// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
/// workflows:
///   - workflowId: w
///     failureActions: [ { name: again, type: retry }, { name: stop, type: end } ]
///     steps:
///       - { stepId: s, operationId: op, onFailure: [ { name: again, type: end } ] }
/// "#)?;
/// let w = &doc.workflows[0];
/// let merged = refs::step_actions(&doc, &w.failure_actions, &w.steps[0].on_failure, false)?;
/// let kinds: Vec<(&str, ActionKind)> = merged.iter().map(|a| (a.name.as_str(), a.kind)).collect();
/// assert_eq!(kinds, [("again", ActionKind::End), ("stop", ActionKind::End)], "the step's `again` wins");
/// # Ok(())
/// # }
/// ```
pub fn step_actions(doc: &Document, workflow: &[ActionOrRef], step: &[ActionOrRef], success: bool) -> Result<Vec<Action>> {
    let mut out: Vec<Action> = step.iter().map(|a| action(doc, a, success)).collect::<Result<_>>()?;
    for a in workflow {
        let a = action(doc, a, success)?;
        if !out.iter().any(|b| b.name == a.name) {
            out.push(a);
        }
    }
    Ok(out)
}
