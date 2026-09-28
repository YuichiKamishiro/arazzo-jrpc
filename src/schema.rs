//! Structural validation: is this a well-formed Arazzo document?
//!
//! Two layers. The official OAI JSON Schema (1.0 or 1.1, embedded) is the
//! arbiter of validity. It reports failures inside `oneOf` branches as
//! "not valid under any of the schemas" at the step, which says nothing
//! useful, so our own checks walk the same objects and name the offending
//! field. A document is valid when both layers agree it has no problems.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde_json::{Map, Value};

use crate::error::Problem;

const SCHEMA_1_0: &str = include_str!("../schemas/arazzo-1.0.json");
const SCHEMA_1_1: &str = include_str!("../schemas/arazzo-1.1.json");

/// Every structural problem of a parsed document, own checks first.
///
/// Empty for a valid document.
pub fn structural(doc: &Value) -> Vec<Problem> {
    let Some(root) = doc.as_object() else {
        return vec![Problem::new("", "Arazzo document must be an object")];
    };
    let validator = match root.get("arazzo").and_then(Value::as_str) {
        Some(v) if v.starts_with("1.0.") => official(SCHEMA_1_0),
        Some(v) if v.starts_with("1.1.") => official(SCHEMA_1_1),
        Some(v) => return vec![Problem::new("/arazzo", format!("version {v:?} is not supported: 1.0.x or 1.1.x required"))],
        None => return vec![Problem::new("/arazzo", "field arazzo is required and must be a version string")],
    };
    let mut problems = Walker::default().document(root);
    let own: BTreeSet<String> = problems.iter().map(|p| p.path.clone()).collect();
    problems.extend(
        validator
            .iter_errors(doc)
            .map(|e| Problem::new(e.instance_path().to_string(), e.to_string()))
            .filter(|p| !own.iter().any(|o| p.path.starts_with(o.as_str()))),
    );
    problems
}

/// The compiled official schema for one version, built once per process.
fn official(text: &'static str) -> &'static jsonschema::Validator {
    static V1_0: OnceLock<jsonschema::Validator> = OnceLock::new();
    static V1_1: OnceLock<jsonschema::Validator> = OnceLock::new();
    let cell = if std::ptr::eq(text, SCHEMA_1_0) { &V1_0 } else { &V1_1 };
    cell.get_or_init(|| {
        let schema: Value = serde_json::from_str(text).expect("embedded Arazzo schema is valid JSON");
        jsonschema::draft202012::new(&schema).expect("embedded Arazzo schema compiles")
    })
}

const STEP_FIELDS: &[&str] = &[
    "stepId", "description", "operationId", "operationPath", "channelPath", "workflowId", "parameters",
    "requestBody", "successCriteria", "onSuccess", "onFailure", "outputs", "timeout", "correlationId",
    "action", "dependsOn",
];
const TARGETS: &[&str] = &["operationId", "operationPath", "channelPath", "workflowId"];

/// Our checks: one pass over the document collecting problems with paths.
#[derive(Default)]
struct Walker {
    problems: Vec<Problem>,
}

impl Walker {
    fn push(&mut self, path: &str, message: impl Into<String>) {
        self.problems.push(Problem::new(path, message));
    }

    /// Walks workflows (unique `workflowId`) and components; returns every problem.
    fn document(mut self, root: &Map<String, Value>) -> Vec<Problem> {
        let mut workflow_ids = BTreeSet::new();
        for (i, w) in array(root.get("workflows")).iter().enumerate() {
            let path = format!("/workflows/{i}");
            let Some(w) = w.as_object() else { continue };
            if let Some(id) = w.get("workflowId").and_then(Value::as_str) {
                if !workflow_ids.insert(id) {
                    self.push(&path, format!("workflowId {id:?} is duplicated"));
                }
            }
            self.workflow(&path, w);
        }
        if let Some(c) = root.get("components").and_then(Value::as_object) {
            self.components(c);
        }
        self.problems
    }

    /// Component keys match `^[a-zA-Z0-9.\-_]+$`; actions and parameters are checked as inline ones.
    fn components(&mut self, c: &Map<String, Value>) {
        for (kind, map) in c.iter().filter(|(k, _)| !k.starts_with("x-")) {
            for (key, v) in map.as_object().into_iter().flatten() {
                let at = format!("/components/{kind}/{key}");
                if !is_key(key) {
                    self.push(&at, format!("key {key:?} does not match ^[a-zA-Z0-9.\\-_]+$"));
                }
                match kind.as_str() {
                    "successActions" | "failureActions" => self.action(&at, v),
                    "parameters" => self.parameter(&at, v, false),
                    _ => {}
                }
            }
        }
    }

    /// Steps (unique `stepId`), then workflow parameters, actions and outputs.
    fn workflow(&mut self, path: &str, w: &Map<String, Value>) {
        let mut step_ids = BTreeSet::new();
        for (j, s) in array(w.get("steps")).iter().enumerate() {
            let at = format!("{path}/steps/{j}");
            let Some(s) = s.as_object() else { continue };
            if let Some(id) = s.get("stepId").and_then(Value::as_str) {
                if !step_ids.insert(id) {
                    self.push(&at, format!("stepId {id:?} is duplicated"));
                }
            }
            self.step(&at, s);
        }
        self.parameters(&format!("{path}/parameters"), w.get("parameters"), false);
        for key in ["successActions", "failureActions"] {
            for (k, a) in array(w.get(key)).iter().enumerate() {
                self.action(&format!("{path}/{key}/{k}"), a);
            }
        }
        self.outputs(&format!("{path}/outputs"), w.get("outputs"));
    }

    /// One step; the order of checks is the order of problems in the report.
    fn step(&mut self, at: &str, s: &Map<String, Value>) {
        let name = s.get("stepId").and_then(Value::as_str).unwrap_or("?");
        let is_workflow = s.contains_key("workflowId");
        self.step_fields(at, name, s);
        self.step_action(at, name, s, is_workflow);
        self.success_criteria(at, name, s);
        self.parameters(&format!("{at}/parameters"), s.get("parameters"), !is_workflow);
        self.depends_on(at, name, s.get("dependsOn"));
        for key in ["onSuccess", "onFailure"] {
            for (k, a) in array(s.get(key)).iter().enumerate() {
                self.action(&format!("{at}/{key}/{k}"), a);
            }
        }
        self.outputs(&format!("{at}/outputs"), s.get("outputs"));
        if let Some(rb) = s.get("requestBody").and_then(Value::as_object) {
            self.request_body(&format!("{at}/requestBody"), rb);
        }
    }

    /// No unknown fields; exactly one of the four targets.
    fn step_fields(&mut self, at: &str, name: &str, s: &Map<String, Value>) {
        if let Some(unknown) = s.keys().find(|k| !k.starts_with("x-") && !STEP_FIELDS.contains(&k.as_str())) {
            self.push(at, format!("step {name}: unknown field {unknown}"));
        }
        let targets: Vec<&str> = TARGETS.iter().copied().filter(|t| s.contains_key(*t)).collect();
        match targets.as_slice() {
            [] => self.push(at, format!("step {name}: one of operationId, operationPath, channelPath, workflowId is required")),
            [_] => {}
            many => self.push(at, format!("step {name}: {} are mutually exclusive", many.join(" and "))),
        }
    }

    /// `action` only on AsyncAPI steps (required with `channelPath`);
    /// `correlationId` only with `action: receive`.
    fn step_action(&mut self, at: &str, name: &str, s: &Map<String, Value>, is_workflow: bool) {
        match s.get("action") {
            None if s.contains_key("channelPath") => self.push(at, format!("step {name}: channelPath requires action (send|receive)")),
            Some(_) if is_workflow => self.push(at, format!("step {name}: action does not apply to a workflowId step")),
            Some(_) if s.contains_key("operationPath") => self.push(at, format!("step {name}: action does not apply to operationPath (OpenAPI)")),
            Some(a) if a != "send" && a != "receive" => self.push(at, format!("step {name}: action must be send or receive, not {a}")),
            _ => {}
        }
        if s.contains_key("correlationId") && s.get("action").and_then(Value::as_str) != Some("receive") {
            self.push(at, format!("step {name}: correlationId is allowed only with action: receive"));
        }
    }

    /// `successCriteria`, when present, is non-empty and each criterion is well-formed.
    fn success_criteria(&mut self, at: &str, name: &str, s: &Map<String, Value>) {
        let criteria = array(s.get("successCriteria"));
        if criteria.is_empty() && s.contains_key("successCriteria") {
            self.push(at, format!("step {name}: successCriteria cannot be empty"));
        }
        for (k, c) in criteria.iter().enumerate() {
            self.criterion(&format!("{at}/successCriteria/{k}"), c);
        }
    }

    /// Payload replacements: a valid `targetSelectorType` and well-formed selector values.
    fn request_body(&mut self, at: &str, rb: &Map<String, Value>) {
        for (k, r) in array(rb.get("replacements")).iter().enumerate() {
            let rat = format!("{at}/replacements/{k}");
            if let Some(t) = r.get("targetSelectorType") {
                self.expression_type(&format!("{rat}/targetSelectorType"), t, "targetSelectorType");
            }
            if let Some(v) = r.get("value").filter(|v| v.is_object()) {
                self.selector(&format!("{rat}/value"), v);
            }
        }
    }

    /// `dependsOn`, when present: non-empty, no duplicates, `$…` entries are step references.
    fn depends_on(&mut self, at: &str, name: &str, deps: Option<&Value>) {
        let Some(deps) = deps else { return };
        let list = array(Some(deps));
        if list.is_empty() {
            self.push(at, format!("step {name}: dependsOn cannot be empty"));
        }
        let mut seen = BTreeSet::new();
        for d in list.iter().filter_map(Value::as_str) {
            if !seen.insert(d) {
                self.push(at, format!("step {name}: dependsOn: {d} is duplicated"));
            }
            if d.starts_with('$') && !is_step_reference(d) {
                self.push(
                    at,
                    format!("step {name}: dependsOn {d}: expected stepId, $workflows.<id>.steps.<id> or $sourceDescriptions.<name>.<id>.steps.<id>"),
                );
            }
        }
    }

    /// `in` is required for operation parameters, forbidden nowhere else;
    /// a parameter is identified by `name` + `in`.
    fn parameters(&mut self, at: &str, list: Option<&Value>, need_in: bool) {
        let mut seen = BTreeSet::new();
        for (k, p) in array(list).iter().enumerate() {
            let pat = format!("{at}/{k}");
            if p.get("reference").is_some() {
                continue;
            }
            self.parameter(&pat, p, need_in);
            let key = (p.get("name").cloned(), p.get("in").cloned());
            if !seen.insert(serde_json::to_string(&key).unwrap_or_default()) {
                self.push(at, format!("parameters: duplicate {}", p.get("name").unwrap_or(&Value::Null)));
            }
        }
    }

    /// One inline parameter: `in` when required, `value` present, selector values well-formed.
    fn parameter(&mut self, at: &str, p: &Value, need_in: bool) {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        if need_in && p.get("in").is_none() {
            self.push(at, format!("parameter {name}: an operation requires in"));
        }
        match p.get("value") {
            None => self.push(at, format!("parameter {name}: value is required")),
            Some(v) if v.is_object() => self.selector(&format!("{at}/value"), v),
            Some(_) => {}
        }
    }

    /// One inline action: goto target, stepId/workflowId exclusive, retry fields, criteria.
    fn action(&mut self, at: &str, a: &Value) {
        if a.get("reference").is_some() {
            return;
        }
        let name = a.get("name").and_then(Value::as_str).unwrap_or("?");
        let (step, wf) = (a.get("stepId").is_some(), a.get("workflowId").is_some());
        if a.get("type").and_then(Value::as_str) == Some("goto") && !step && !wf {
            self.push(at, format!("action {name}: goto requires stepId or workflowId"));
        }
        if step && wf {
            self.push(at, format!("action {name}: stepId and workflowId are mutually exclusive"));
        }
        if a.get("parameters").is_some() && !wf {
            self.push(at, format!("action {name}: parameters require workflowId"));
        }
        if let Some(l) = a.get("retryLimit") {
            if l.as_u64().is_none() {
                self.push(at, format!("action {name}: retryLimit must be an integer >= 0, not {l}"));
            }
        }
        if let Some(r) = a.get("retryAfter") {
            if r.as_f64().is_none_or(|r| r < 0.0) {
                self.push(at, format!("action {name}: retryAfter must be a number >= 0, not {r}"));
            }
        }
        for (k, c) in array(a.get("criteria")).iter().enumerate() {
            self.criterion(&format!("{at}/criteria/{k}"), c);
        }
    }

    /// One criterion: `condition` present; a typed one needs `context` and a valid type.
    fn criterion(&mut self, at: &str, c: &Value) {
        if c.get("condition").is_none() {
            self.push(at, "criterion: missing condition");
        }
        let Some(t) = c.get("type") else { return };
        if t == "simple" {
            return;
        }
        if c.get("context").is_none() {
            self.push(at, "a criterion with type requires context");
        }
        if t.is_object() {
            self.expression_type(&format!("{at}/type"), t, "type");
        }
    }

    /// Output keys match `^[a-zA-Z0-9.\-_]+$`; selector values are well-formed.
    fn outputs(&mut self, at: &str, outputs: Option<&Value>) {
        for (k, v) in outputs.and_then(Value::as_object).into_iter().flatten() {
            if !is_key(k) {
                self.push(at, format!("outputs: key {k:?} does not match ^[a-zA-Z0-9.\\-_]+$"));
            }
            if v.is_object() {
                self.selector(&format!("{at}/{k}"), v);
            }
        }
    }

    /// A Selector Object has `context`, `selector` and a valid `type`.
    fn selector(&mut self, at: &str, v: &Value) {
        let missing: Vec<&str> = ["context", "selector", "type"].into_iter().filter(|k| v.get(k).is_none()).collect();
        if !missing.is_empty() {
            self.push(at, format!("Selector Object: missing {}", missing.join(", ")));
        }
        if let Some(t) = v.get("type") {
            self.expression_type(&format!("{at}/type"), t, "type");
        }
    }

    /// A selector type: `jsonpath|xpath|jsonpointer` or `{type, version}`
    /// with a version allowed for that type (5.8.12).
    fn expression_type(&mut self, at: &str, t: &Value, field: &str) {
        let (kind, version) = match t {
            Value::String(s) => (s.as_str(), None),
            Value::Object(o) => (o.get("type").and_then(Value::as_str).unwrap_or(""), o.get("version").and_then(Value::as_str)),
            _ => ("", None),
        };
        let versions: &[&str] = match kind {
            "jsonpath" => &["rfc9535", "draft-goessner-dispatch-jsonpath-00"],
            "xpath" => &["xpath-31", "xpath-30", "xpath-20", "xpath-10"],
            "jsonpointer" => &["rfc6901"],
            other => return self.push(at, format!("{field}: {other:?}: jsonpath, xpath or jsonpointer required")),
        };
        if let Some(v) = version.filter(|v| !versions.contains(v)) {
            self.push(at, format!("{field}: version {v:?} is not allowed for {kind} (allowed: {})", versions.join(", ")));
        }
    }
}

/// The items of a JSON array, or an empty slice for anything else.
fn array(v: Option<&Value>) -> &[Value] {
    v.and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

/// Component and output keys: `^[a-zA-Z0-9.\-_]+$`.
pub(crate) fn is_key(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `$workflows.<id>.steps.<id>` or `$sourceDescriptions.<name>.<id>.steps.<id>`.
pub(crate) fn is_step_reference(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    match parts.as_slice() {
        ["$workflows", w, "steps", st] => !w.is_empty() && !st.is_empty(),
        ["$sourceDescriptions", n, w, "steps", st] => !n.is_empty() && !w.is_empty() && !st.is_empty(),
        _ => false,
    }
}
