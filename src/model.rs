//! Arazzo 1.1 object model (spec 5.8); 1.0 documents map onto the same types.
//!
//! Deserialization is lenient — `Document::parse` has already run the
//! official JSON Schema — so unknown keys here can only be `x-*` and are kept
//! in each object's `extensions`.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// `x-*` keys of an extensible object, in key order.
pub type Extensions = BTreeMap<String, Value>;

/// One Arazzo document (spec 5.8.1).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Document {
    pub arazzo: String,
    #[serde(rename = "$self", default, skip_serializing_if = "Option::is_none")]
    pub self_uri: Option<String>,
    pub info: Info,
    #[serde(rename = "sourceDescriptions")]
    pub source_descriptions: Vec<SourceDescription>,
    pub workflows: Vec<Workflow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<Components>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

impl Document {
    /// Finds a workflow by `workflowId`.
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
    /// workflows: [ { workflowId: mark, steps: [ { stepId: s, operationId: op } ] } ]
    /// "#)?;
    /// assert!(doc.workflow("mark").is_some());
    /// assert!(doc.workflow("nope").is_none());
    /// # Ok(())
    /// # }
    /// ```
    pub fn workflow(&self, id: &str) -> Option<&Workflow> {
        self.workflows.iter().find(|w| w.workflow_id == id)
    }

    /// Finds a source description by `name`.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::model::SourceType;
    /// use arazzo_jrpc::Document;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let doc = Document::parse(r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op } ] } ]
    /// "#)?;
    /// assert_eq!(doc.source("api").map(|s| s.kind), Some(Some(SourceType::Openapi)));
    /// assert!(doc.source("nope").is_none());
    /// # Ok(())
    /// # }
    /// ```
    pub fn source(&self, name: &str) -> Option<&SourceDescription> {
        self.source_descriptions.iter().find(|s| s.name == name)
    }
}

/// Info Object (5.8.2).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Info {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub version: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The `type` of a source description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    Openapi,
    Asyncapi,
    Arazzo,
}

/// Source Description Object (5.8.3).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SourceDescription {
    pub name: String,
    pub url: String,
    /// Absent means opaque: the caller's invoker serves it (spec: optional).
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SourceType>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Workflow Object (5.8.4).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Workflow {
    #[serde(rename = "workflowId")]
    pub workflow_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema 2020-12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<Value>,
    #[serde(rename = "dependsOn", default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    pub steps: Vec<Step>,
    #[serde(rename = "successActions", default, skip_serializing_if = "Vec::is_empty")]
    pub success_actions: Vec<ActionOrRef>,
    #[serde(rename = "failureActions", default, skip_serializing_if = "Vec::is_empty")]
    pub failure_actions: Vec<ActionOrRef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, ValueOrSelector>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<ParameterOrRef>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

impl Workflow {
    /// Finds a step by `stepId`.
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
    /// workflows: [ { workflowId: w, steps: [ { stepId: login, operationId: op }, { stepId: fetch, operationId: op2 } ] } ]
    /// "#)?;
    /// let w = &doc.workflows[0];
    /// assert_eq!(w.step("fetch").map(|s| s.step_id.as_str()), Some("fetch"));
    /// assert!(w.step("nope").is_none());
    /// # Ok(())
    /// # }
    /// ```
    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.step_id == id)
    }

    /// Finds the position of a step by `stepId`.
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
    /// workflows: [ { workflowId: w, steps: [ { stepId: login, operationId: op }, { stepId: fetch, operationId: op2 } ] } ]
    /// "#)?;
    /// assert_eq!(doc.workflows[0].step_index("fetch"), Some(1));
    /// assert_eq!(doc.workflows[0].step_index("nope"), None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn step_index(&self, id: &str) -> Option<usize> {
        self.steps.iter().position(|s| s.step_id == id)
    }
}

/// What a step invokes; the four fields are mutually exclusive.
#[derive(Debug, Clone, PartialEq)]
pub enum StepTarget {
    OperationId(String),
    OperationPath(String),
    ChannelPath(String),
    WorkflowId(String),
}

/// The `action` of an AsyncAPI step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AsyncAction {
    Send,
    Receive,
}

/// Step Object (5.8.5); (de)serialized through `RawStep`, which keeps the
/// four target fields apart.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(try_from = "RawStep", into = "RawStep")]
pub struct Step {
    pub step_id: String,
    pub description: Option<String>,
    pub target: StepTarget,
    pub parameters: Vec<ParameterOrRef>,
    pub request_body: Option<RequestBody>,
    pub success_criteria: Vec<Criterion>,
    pub on_success: Vec<ActionOrRef>,
    pub on_failure: Vec<ActionOrRef>,
    pub outputs: BTreeMap<String, ValueOrSelector>,
    /// Milliseconds.
    pub timeout: Option<u64>,
    pub correlation_id: Option<Value>,
    pub action: Option<AsyncAction>,
    pub depends_on: Vec<String>,
    pub extensions: Extensions,
}

/// Where a parameter goes (`in`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterIn {
    Path,
    Query,
    Querystring,
    Header,
    Cookie,
    Channel,
}

/// Parameter Object (5.8.6).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Parameter {
    pub name: String,
    /// Required for operation steps; absent when the parameter feeds
    /// workflow inputs.
    #[serde(rename = "in", default, skip_serializing_if = "Option::is_none")]
    pub location: Option<ParameterIn>,
    pub value: ValueOrSelector,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Reusable Object: a `$components.*` reference. Unknown properties are
/// ignored, as the spec requires.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Reusable {
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

/// A parameter list entry: a `$components` reference or an inline parameter.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ParameterOrRef {
    Reference(Reusable),
    Parameter(Parameter),
}

impl ParameterOrRef {
    /// Tells an inline parameter from a `$components` reference.
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
    /// workflows:
    ///   - workflowId: w
    ///     steps:
    ///       - stepId: s
    ///         operationId: op
    ///         parameters:
    ///           - { name: petId, in: path, value: 1 }
    ///           - { reference: $components.parameters.page }
    /// components: { parameters: { page: { name: page, in: query, value: 1 } } }
    /// "#)?;
    /// let p = &doc.workflows[0].steps[0].parameters;
    /// assert_eq!(p[0].as_parameter().map(|p| p.name.as_str()), Some("petId"));
    /// assert!(p[1].as_parameter().is_none());
    /// # Ok(())
    /// # }
    /// ```
    pub fn as_parameter(&self) -> Option<&Parameter> {
        match self {
            ParameterOrRef::Parameter(p) => Some(p),
            ParameterOrRef::Reference(_) => None,
        }
    }

    /// Tells a `$components` reference from an inline parameter.
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
    /// workflows:
    ///   - workflowId: w
    ///     steps:
    ///       - stepId: s
    ///         operationId: op
    ///         parameters:
    ///           - { name: petId, in: path, value: 1 }
    ///           - { reference: $components.parameters.page, value: 2 }
    /// components: { parameters: { page: { name: page, in: query, value: 1 } } }
    /// "#)?;
    /// let p = &doc.workflows[0].steps[0].parameters;
    /// assert!(p[0].as_reference().is_none());
    /// let r = p[1].as_reference().expect("a reference");
    /// assert_eq!((r.reference.as_str(), r.value.as_ref()), ("$components.parameters.page", Some(&2.into())));
    /// # Ok(())
    /// # }
    /// ```
    pub fn as_reference(&self) -> Option<&Reusable> {
        match self {
            ParameterOrRef::Reference(r) => Some(r),
            ParameterOrRef::Parameter(_) => None,
        }
    }
}

/// The `type` of a success or failure action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionKind {
    End,
    Goto,
    Retry,
}

/// Success Action and Failure Action share one shape; `retry`,
/// `retryAfter` and `retryLimit` only occur on failure actions.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Action {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ActionKind,
    #[serde(rename = "workflowId", default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(rename = "stepId", default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<ParameterOrRef>,
    #[serde(rename = "retryAfter", default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<f64>,
    #[serde(rename = "retryLimit", default, skip_serializing_if = "Option::is_none")]
    pub retry_limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<Criterion>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// An action list entry: a `$components` reference or an inline action.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ActionOrRef {
    Reference(Reusable),
    Action(Action),
}

impl ActionOrRef {
    /// Tells an inline action from a `$components` reference.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::model::ActionKind;
    /// use arazzo_jrpc::Document;
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
    ///         onSuccess:
    ///           - { name: done, type: end }
    ///           - { reference: $components.successActions.stop }
    /// components: { successActions: { stop: { name: stop, type: end } } }
    /// "#)?;
    /// let a = &doc.workflows[0].steps[0].on_success;
    /// assert_eq!(a[0].as_action().map(|a| a.kind), Some(ActionKind::End));
    /// assert!(a[1].as_action().is_none());
    /// # Ok(())
    /// # }
    /// ```
    pub fn as_action(&self) -> Option<&Action> {
        match self {
            ActionOrRef::Action(a) => Some(a),
            ActionOrRef::Reference(_) => None,
        }
    }

    /// Tells a `$components` reference from an inline action.
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
    /// workflows:
    ///   - workflowId: w
    ///     steps:
    ///       - stepId: s
    ///         operationId: op
    ///         onFailure:
    ///           - { reference: $components.failureActions.again }
    /// components: { failureActions: { again: { name: again, type: retry } } }
    /// "#)?;
    /// let a = &doc.workflows[0].steps[0].on_failure;
    /// assert_eq!(a[0].as_reference().map(|r| r.reference.as_str()), Some("$components.failureActions.again"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn as_reference(&self) -> Option<&Reusable> {
        match self {
            ActionOrRef::Reference(r) => Some(r),
            ActionOrRef::Action(_) => None,
        }
    }
}

/// Selector kinds (5.8.12): JSONPath, XPath, JSON Pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SelectorKind {
    Jsonpath,
    Xpath,
    Jsonpointer,
}

/// A selector type: bare kind or an Expression Type Object with a version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionType {
    pub kind: SelectorKind,
    /// `None` means the default for the kind (rfc9535, xpath-31, rfc6901).
    pub version: Option<String>,
}

impl ExpressionType {
    /// The version in effect, defaulted per kind.
    ///
    /// Defaults to `rfc9535`, `xpath-31` or `rfc6901` by type.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::model::{ExpressionType, SelectorKind};
    ///
    /// let xpath = ExpressionType { kind: SelectorKind::Xpath, version: None };
    /// assert_eq!(xpath.version(), "xpath-31");
    /// let old = ExpressionType { kind: SelectorKind::Xpath, version: Some("xpath-10".into()) };
    /// assert_eq!(old.version(), "xpath-10");
    /// ```
    pub fn version(&self) -> &str {
        self.version.as_deref().unwrap_or(match self.kind {
            SelectorKind::Jsonpath => "rfc9535",
            SelectorKind::Xpath => "xpath-31",
            SelectorKind::Jsonpointer => "rfc6901",
        })
    }
}

impl<'de> Deserialize<'de> for ExpressionType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let (kind, version) = match RawType::deserialize(d)? {
            RawType::Name(k) => (k, None),
            RawType::Object { kind, version, .. } => (kind, version),
        };
        Ok(ExpressionType {
            kind: selector_kind(&kind).map_err(serde::de::Error::custom)?,
            version,
        })
    }
}

impl Serialize for ExpressionType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let kind = serde_json::to_value(self.kind).unwrap().as_str().unwrap().to_string();
        match &self.version {
            None => RawType::Name(kind).serialize(s),
            Some(v) => RawType::Object { kind, version: Some(v.clone()), _extensions: Extensions::new() }.serialize(s),
        }
    }
}

/// Selector Object (5.8.13).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Selector {
    pub context: String,
    pub selector: String,
    #[serde(rename = "type")]
    pub kind: ExpressionType,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A value that is either a Selector Object or anything else (literal or
/// runtime expression).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ValueOrSelector {
    Selector(Selector),
    Value(Value),
}

impl ValueOrSelector {
    /// Tells a Selector Object from a plain value.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::model::ValueOrSelector;
    /// use serde_json::json;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let selector: ValueOrSelector = serde_json::from_value(json!({"context": "$inputs.user", "selector": "$.id", "type": "jsonpath"}))?;
    /// assert!(selector.is_selector());
    /// let plain: ValueOrSelector = serde_json::from_value(json!("$inputs.id"))?;
    /// assert!(!plain.is_selector());
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_selector(&self) -> bool {
        matches!(self, ValueOrSelector::Selector(_))
    }
}

/// Criterion type: simple/regex or a selector kind (with an optional version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CriterionType {
    Simple,
    Regex,
    Expression(ExpressionType),
}

impl<'de> Deserialize<'de> for CriterionType {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match RawType::deserialize(d)? {
            RawType::Name(n) if n == "simple" => Ok(CriterionType::Simple),
            RawType::Name(n) if n == "regex" => Ok(CriterionType::Regex),
            RawType::Name(n) => Ok(CriterionType::Expression(ExpressionType {
                kind: selector_kind(&n).map_err(serde::de::Error::custom)?,
                version: None,
            })),
            RawType::Object { kind, version, .. } => Ok(CriterionType::Expression(ExpressionType {
                kind: selector_kind(&kind).map_err(serde::de::Error::custom)?,
                version,
            })),
        }
    }
}

impl Serialize for CriterionType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CriterionType::Simple => s.serialize_str("simple"),
            CriterionType::Regex => s.serialize_str("regex"),
            CriterionType::Expression(e) => e.serialize(s),
        }
    }
}

/// Criterion Object (5.8.11).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Criterion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub condition: String,
    #[serde(rename = "type", default = "simple", skip_serializing_if = "is_simple")]
    pub kind: CriterionType,
    #[serde(flatten)]
    pub extensions: Extensions,
}

impl Criterion {
    /// Builds a simple criterion without context.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::model::{Criterion, CriterionType};
    ///
    /// let c = Criterion::simple("$statusCode == 200");
    /// assert_eq!(c.kind, CriterionType::Simple);
    /// assert_eq!(c.context, None);
    /// ```
    pub fn simple(condition: &str) -> Self {
        Criterion {
            context: None,
            condition: condition.to_string(),
            kind: CriterionType::Simple,
            extensions: Extensions::new(),
        }
    }
}

/// Request Body Object (5.8.14).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RequestBody {
    #[serde(rename = "contentType", default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Literal, runtime expression, template string, or structure that may
    /// hold expressions and Selector Objects anywhere inside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replacements: Vec<PayloadReplacement>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Payload Replacement Object (5.8.15).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PayloadReplacement {
    pub target: String,
    #[serde(rename = "targetSelectorType", default, skip_serializing_if = "Option::is_none")]
    pub target_selector_type: Option<ExpressionType>,
    pub value: ValueOrSelector,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Components Object (5.8.9).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Components {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, Parameter>,
    #[serde(rename = "successActions", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub success_actions: BTreeMap<String, Action>,
    #[serde(rename = "failureActions", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failure_actions: BTreeMap<String, Action>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

// ---------------------------------------------------------------- serde representations

/// The document shape of a Step; `Step` holds the target as one enum.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct RawStep {
    #[serde(rename = "stepId")]
    step_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(rename = "operationId", default, skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
    #[serde(rename = "operationPath", default, skip_serializing_if = "Option::is_none")]
    operation_path: Option<String>,
    #[serde(rename = "channelPath", default, skip_serializing_if = "Option::is_none")]
    channel_path: Option<String>,
    #[serde(rename = "workflowId", default, skip_serializing_if = "Option::is_none")]
    workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parameters: Vec<ParameterOrRef>,
    #[serde(rename = "requestBody", default, skip_serializing_if = "Option::is_none")]
    request_body: Option<RequestBody>,
    #[serde(rename = "successCriteria", default, skip_serializing_if = "Vec::is_empty")]
    success_criteria: Vec<Criterion>,
    #[serde(rename = "onSuccess", default, skip_serializing_if = "Vec::is_empty")]
    on_success: Vec<ActionOrRef>,
    #[serde(rename = "onFailure", default, skip_serializing_if = "Vec::is_empty")]
    on_failure: Vec<ActionOrRef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    outputs: BTreeMap<String, ValueOrSelector>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout: Option<u64>,
    #[serde(rename = "correlationId", default, skip_serializing_if = "Option::is_none")]
    correlation_id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action: Option<AsyncAction>,
    #[serde(rename = "dependsOn", default, skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    #[serde(flatten)]
    extensions: Extensions,
}

/// Exactly one of the four target fields must be set.
impl TryFrom<RawStep> for Step {
    type Error = String;

    fn try_from(r: RawStep) -> Result<Self, String> {
        let targets: Vec<StepTarget> = [
            r.operation_id.map(StepTarget::OperationId),
            r.operation_path.map(StepTarget::OperationPath),
            r.channel_path.map(StepTarget::ChannelPath),
            r.workflow_id.map(StepTarget::WorkflowId),
        ]
        .into_iter()
        .flatten()
        .collect();
        let [target] = <[StepTarget; 1]>::try_from(targets).map_err(|t| {
            format!(
                "step {}: exactly one of operationId, operationPath, channelPath, workflowId is required ({} given)",
                r.step_id,
                t.len()
            )
        })?;
        Ok(Step {
            step_id: r.step_id,
            description: r.description,
            target,
            parameters: r.parameters,
            request_body: r.request_body,
            success_criteria: r.success_criteria,
            on_success: r.on_success,
            on_failure: r.on_failure,
            outputs: r.outputs,
            timeout: r.timeout,
            correlation_id: r.correlation_id,
            action: r.action,
            depends_on: r.depends_on,
            extensions: r.extensions,
        })
    }
}

impl From<Step> for RawStep {
    fn from(s: Step) -> Self {
        let mut r = RawStep {
            step_id: s.step_id,
            description: s.description,
            parameters: s.parameters,
            request_body: s.request_body,
            success_criteria: s.success_criteria,
            on_success: s.on_success,
            on_failure: s.on_failure,
            outputs: s.outputs,
            timeout: s.timeout,
            correlation_id: s.correlation_id,
            action: s.action,
            depends_on: s.depends_on,
            extensions: s.extensions,
            ..Default::default()
        };
        match s.target {
            StepTarget::OperationId(v) => r.operation_id = Some(v),
            StepTarget::OperationPath(v) => r.operation_path = Some(v),
            StepTarget::ChannelPath(v) => r.channel_path = Some(v),
            StepTarget::WorkflowId(v) => r.workflow_id = Some(v),
        }
        r
    }
}

/// A `type` field as written: a bare name or an Expression Type Object.
#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum RawType {
    Name(String),
    Object {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(flatten)]
        _extensions: Extensions,
    },
}

fn selector_kind(s: &str) -> Result<SelectorKind, String> {
    match s {
        "jsonpath" => Ok(SelectorKind::Jsonpath),
        "xpath" => Ok(SelectorKind::Xpath),
        "jsonpointer" => Ok(SelectorKind::Jsonpointer),
        other => Err(format!("unknown expression type {other:?}")),
    }
}

/// Serde default of `Criterion::kind`.
fn simple() -> CriterionType {
    CriterionType::Simple
}

fn is_simple(t: &CriterionType) -> bool {
    *t == CriterionType::Simple
}
