//! One Arazzo document: parsing and the checks that need nothing but the
//! document itself.

use serde_json::Value;

use crate::error::{Error, Problem, Result};
use crate::model::{Document, SourceType, Step, StepTarget};
use crate::schema;

/// The runtime-expression prefix that names a source description.
pub const SOURCE_PREFIX: &str = "$sourceDescriptions.";

/// Parses YAML or JSON text into a tree, nothing more.
///
/// # Errors
///
/// The text is not YAML or JSON.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::document::parse_value;
/// use serde_json::json;
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// assert_eq!(parse_value("a: 1\nb: [x]\n")?, json!({"a": 1, "b": ["x"]}));
/// assert_eq!(parse_value(r#"{"a": 1}"#)?, json!({"a": 1}));
/// assert!(parse_value(": : :").is_err());
/// # Ok(())
/// # }
/// ```
pub fn parse_value(text: &str) -> Result<Value> {
    serde_yaml_ng::from_str(text).map_err(|e| Error(format!("not YAML/JSON: {e}")))
}

impl Document {
    /// Parses and structurally validates one Arazzo document (1.0.x or 1.1.x).
    ///
    /// # Errors
    ///
    /// The text is not YAML/JSON or does not match the Arazzo schema; the message
    /// lists every structural problem, one per line.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Document;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let doc = Document::parse(r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op } ] } ]
    /// "#)?;
    /// assert_eq!(doc.workflows[0].steps[0].step_id, "s");
    ///
    /// let e = Document::parse("arazzo: 1.1.0\n").unwrap_err();
    /// assert!(e.to_string().contains("info"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn parse(text: &str) -> Result<Document> {
        Self::from_value(parse_value(text)?)
    }

    /// Validates an already parsed tree and deserializes it.
    ///
    /// # Errors
    ///
    /// As for [`Document::parse`].
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Document;
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let doc = Document::from_value(json!({
    ///     "arazzo": "1.1.0",
    ///     "info": {"title": "t", "version": "1"},
    ///     "sourceDescriptions": [{"name": "api", "url": "api.yaml", "type": "openapi"}],
    ///     "workflows": [{"workflowId": "w", "steps": [{"stepId": "s", "operationId": "op"}]}]
    /// }))?;
    /// assert_eq!(doc.workflows[0].workflow_id, "w");
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_value(value: Value) -> Result<Document> {
        let problems = schema::structural(&value);
        if !problems.is_empty() {
            return Err(Error::from_problems(&problems));
        }
        serde_json::from_value(value).map_err(|e| Error(format!("Arazzo document: {e}")))
    }

    /// Semantic checks that need only this document: does each step fit
    /// the `type` of the source it addresses? The official schema cannot
    /// see source types when it picks a step branch.
    ///
    /// Empty when clean.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Document;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let doc = Document::parse(r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op, action: send } ] } ]
    /// "#)?;
    /// let problems = doc.lint();
    /// assert_eq!(problems.len(), 1);
    /// assert!(problems[0].message.contains("action"), "action is for AsyncAPI steps");
    /// # Ok(())
    /// # }
    /// ```
    pub fn lint(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        for (i, w) in self.workflows.iter().enumerate() {
            for (j, s) in w.steps.iter().enumerate() {
                lint_step(self, &format!("/workflows/{i}/steps/{j}"), s, &mut out);
            }
        }
        out
    }

    /// The source a step addresses, if it can be told from the document.
    ///
    /// The name comes from `$sourceDescriptions.<name>.…`, from a
    /// `{$sourceDescriptions.<name>.url}#…` path, or is the single non-Arazzo source
    /// when a bare `operationId` leaves only one candidate. `None` for workflow steps
    /// and ambiguous bare ids.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Document;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let doc = Document::parse(r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions:
    ///   - { name: api, url: api.yaml, type: openapi }
    ///   - { name: flows, url: flows.arazzo.yaml, type: arazzo }
    /// workflows:
    ///   - workflowId: w
    ///     steps:
    ///       - { stepId: a, operationId: op }
    ///       - { stepId: b, operationPath: '{$sourceDescriptions.api.url}#/paths/~1pets/get' }
    ///       - { stepId: c, workflowId: $sourceDescriptions.flows.other }
    /// "#)?;
    /// let steps = &doc.workflows[0].steps;
    /// assert_eq!(doc.step_source(&steps[0]), Some("api"), "the only operation source");
    /// assert_eq!(doc.step_source(&steps[1]), Some("api"));
    /// assert_eq!(doc.step_source(&steps[2]), None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn step_source<'a>(&'a self, step: &'a Step) -> Option<&'a str> {
        match &step.target {
            StepTarget::OperationId(id) => match id.strip_prefix(SOURCE_PREFIX) {
                Some(rest) => rest.split('.').next(),
                None => self.single_operation_source(),
            },
            StepTarget::OperationPath(p) => path_source(p),
            // A bare channelPath is a channel name in the only source (the OAI suite accepts this).
            StepTarget::ChannelPath(p) => path_source(p).or_else(|| self.single_operation_source()),
            StepTarget::WorkflowId(_) => None,
        }
    }

    /// Finds the only source that holds operations (openapi, asyncapi or
    /// untyped).
    ///
    /// `None` unless there is exactly one.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Document;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let doc = Document::parse(r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions:
    ///   - { name: api, url: api.yaml, type: openapi }
    ///   - { name: flows, url: flows.arazzo.yaml, type: arazzo }
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op } ] } ]
    /// "#)?;
    /// assert_eq!(doc.single_operation_source(), Some("api"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn single_operation_source(&self) -> Option<&str> {
        let mut ops = self.source_descriptions.iter().filter(|s| s.kind != Some(SourceType::Arazzo));
        match (ops.next(), ops.next()) {
            (Some(s), None) => Some(&s.name),
            _ => None,
        }
    }
}

/// Splits an operationPath / channelPath into source name and pointer.
///
/// Accepts the form with or without braces and `.url`; `None` for anything else.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::document::split_source_path;
///
/// let expected = Some(("api", "/paths/~1pets/get"));
/// assert_eq!(split_source_path("{$sourceDescriptions.api.url}#/paths/~1pets/get"), expected);
/// assert_eq!(split_source_path("$sourceDescriptions.api#/paths/~1pets/get"), expected);
/// assert_eq!(split_source_path("/paths/~1pets/get"), None);
/// ```
pub fn split_source_path(p: &str) -> Option<(&str, &str)> {
    let (head, pointer) = p.split_once('#')?;
    let head = head.strip_prefix('{').and_then(|h| h.strip_suffix('}')).unwrap_or(head);
    let rest = head.strip_prefix(SOURCE_PREFIX)?;
    let name = rest.strip_suffix(".url").unwrap_or(rest);
    (!name.is_empty() && !name.contains('.') && pointer.starts_with('/')).then_some((name, pointer))
}

/// The source name of an operationPath / channelPath, if it has one.
fn path_source(p: &str) -> Option<&str> {
    split_source_path(p).map(|(name, _)| name)
}

/// Checks one step against the `type` of the source it addresses.
fn lint_step(doc: &Document, at: &str, s: &Step, out: &mut Vec<Problem>) {
    let mut bad = |m: String| out.push(Problem::new(at, format!("step {}: {m}", s.step_id)));
    match &s.target {
        StepTarget::WorkflowId(_) => {
            if s.request_body.is_some() {
                bad("requestBody does not apply to a workflowId step: parameters go to inputs".into());
            }
            return;
        }
        StepTarget::OperationPath(p) if path_source(p).is_none() => {
            bad(format!("operationPath {p:?}: expected {{$sourceDescriptions.<name>.url}}#/<pointer>"));
        }
        _ => {}
    }
    let kind = doc.step_source(s).and_then(|n| doc.source(n)).and_then(|src| src.kind);
    match kind {
        Some(SourceType::Asyncapi) if s.action.is_none() => bad("action is required for an AsyncAPI step (send|receive)".into()),
        Some(SourceType::Openapi) => {
            if s.action.is_some() {
                bad("action does not apply to an OpenAPI step".into());
            }
            if s.correlation_id.is_some() {
                bad("correlationId does not apply to an OpenAPI step".into());
            }
            if matches!(s.target, StepTarget::ChannelPath(_)) {
                bad("channelPath does not apply to an OpenAPI source".into());
            }
        }
        _ => {}
    }
}
