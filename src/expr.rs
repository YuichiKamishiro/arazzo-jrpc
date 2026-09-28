//! Runtime expressions (spec 5.9): parsing, the scope they read from,
//! evaluation, and embedding into strings.
//!
//! Beyond the ABNF, two forms the spec's own examples and test suite use:
//! property access after a value (`$message.payload.orderId`,
//! `$response.body.items[0].id`) and `$workflows.<id>.steps.<id>.outputs.<name>`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::description::{Description, Target};
use crate::error::{Error, Result};
use crate::model::Document;
use crate::transport::{header, Exchange, Message};

/// Where in an HTTP message or AsyncAPI message a `$request`/`$response`/
/// `$message` expression reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Header(String),
    Query(String),
    Path(String),
    Body,
    Payload,
}

/// A property accessor after the value: `.key` or `[index]`.
#[derive(Debug, Clone, PartialEq)]
pub enum Access {
    Key(String),
    Index(usize),
}

/// A parsed runtime expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub root: Root,
    /// `#/json/pointer`, already validated.
    pub pointer: Option<String>,
    /// `.key` / `[i]` accessors after the value.
    pub path: Vec<Access>,
}

/// What an expression reads, before the pointer and accessors.
#[derive(Debug, Clone, PartialEq)]
pub enum Root {
    Url,
    Method,
    StatusCode,
    SelfUri,
    Request(Part),
    Response(Part),
    Message(Part),
    /// `name` may contain dots; which part is the key is decided when evaluating.
    Inputs(String),
    Outputs(String),
    Steps { step: String, name: String },
    WorkflowInputs { workflow: String, name: String },
    WorkflowOutputs { workflow: String, name: String },
    WorkflowStep { workflow: String, step: String, name: String },
    SourceDescriptions { source: String, reference: String },
    Components { kind: String, name: String },
}

/// State of one workflow run as other expressions see it.
#[derive(Debug, Clone, Default)]
pub struct WorkflowState {
    pub inputs: Map<String, Value>,
    pub outputs: Map<String, Value>,
    pub steps: BTreeMap<String, Map<String, Value>>,
}

/// A source description as `$sourceDescriptions.<name>.*` reads it.
#[derive(Debug, Clone)]
pub struct SourceInfo {
    /// The Source Description Object with `url` resolved to absolute.
    pub fields: Value,
    pub api: Option<Arc<Value>>,
    pub arazzo: Option<Arc<Document>>,
}

/// Everything an expression can read at one point of a run.
///
/// Built by `exec` per step (inputs, outputs so far, the last exchange or
/// message) and by tests directly; looks like
/// `Scope { inputs: {"id": 7}, steps: {"get": {"pet": {…}}}, exchange: Some(…), .. }`.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub inputs: Value,
    pub outputs: Map<String, Value>,
    pub steps: BTreeMap<String, Map<String, Value>>,
    pub workflows: BTreeMap<String, WorkflowState>,
    pub exchange: Option<Exchange>,
    pub message: Option<Message>,
    /// The response body of an opaque call (`Invoker`), or its failure body.
    pub body: Option<Value>,
    pub sources: BTreeMap<String, SourceInfo>,
    pub components: Option<Value>,
    pub self_uri: Option<String>,
}

impl Scope {
    /// Builds the static part of a scope for the entry document: its
    /// sources, components and `$self`.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::{expr::{self, Scope}, Description};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op } ] } ]
    /// "#)?;
    /// let scope = Scope::for_description(&d);
    /// assert_eq!(expr::evaluate("$sourceDescriptions.api.url", &scope)?, json!("https://x/api.yaml"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn for_description(d: &Description) -> Scope {
        Self::for_document(d, 0)
    }

    /// Builds the static part of a scope for document `i` of a description
    /// (components and sources are scoped per document).
    ///
    /// # Panics
    ///
    /// `i` is out of range.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::{expr::{self, Scope}, Description};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// $self: https://example.com/flow.arazzo.yaml
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op } ] } ]
    /// "#)?;
    /// let scope = Scope::for_document(&d, 0);
    /// assert_eq!(expr::evaluate("$self", &scope)?, json!("https://example.com/flow.arazzo.yaml"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn for_document(d: &Description, i: usize) -> Scope {
        let loaded = &d.documents()[i];
        let mut s = Scope {
            self_uri: loaded.doc.self_uri.as_ref().map(|_| loaded.base.to_string()),
            components: loaded.doc.components.as_ref().map(|c| serde_json::to_value(c).unwrap()),
            ..Default::default()
        };
        for sd in &loaded.doc.source_descriptions {
            let resolved = loaded.sources.get(&sd.name);
            let mut fields = serde_json::to_value(sd).unwrap();
            if let Some(r) = resolved {
                fields["url"] = Value::String(r.uri.clone());
            }
            let (api, arazzo) = match resolved.map(|r| &r.target) {
                Some(Target::OpenApi(v) | Target::AsyncApi(v)) => (Some(v.clone()), None),
                Some(Target::Arazzo(k)) => (None, Some(d.documents()[*k].doc.clone())),
                _ => (None, None),
            };
            s.sources.insert(sd.name.clone(), SourceInfo { fields, api, arazzo });
        }
        s
    }

    /// Records a workflow output (`$outputs.<name>`).
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::expr::{self, Scope};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let mut scope = Scope::default();
    /// scope.set_output("token", json!("t1"));
    /// assert_eq!(expr::evaluate("$outputs.token", &scope)?, json!("t1"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_output(&mut self, name: &str, v: Value) {
        self.outputs.insert(name.into(), v);
    }

    /// Records a step output (`$steps.<step>.outputs.<name>`).
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::expr::{self, Scope};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let mut scope = Scope::default();
    /// scope.set_step_output("login", "token", json!("t1"));
    /// assert_eq!(expr::evaluate("$steps.login.outputs.token", &scope)?, json!("t1"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_step_output(&mut self, step: &str, name: &str, v: Value) {
        self.steps.entry(step.into()).or_default().insert(name.into(), v);
    }

    /// Records an input of another workflow (`$workflows.<id>.inputs.<name>`).
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::expr::{self, Scope};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let mut scope = Scope::default();
    /// scope.set_workflow_input("login", "user", json!("neo"));
    /// assert_eq!(expr::evaluate("$workflows.login.inputs.user", &scope)?, json!("neo"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_workflow_input(&mut self, workflow: &str, name: &str, v: Value) {
        self.workflows.entry(workflow.into()).or_default().inputs.insert(name.into(), v);
    }

    /// Records an output of another workflow (`$workflows.<id>.outputs.<name>`).
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::expr::{self, Scope};
    /// use serde_json::json;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let mut scope = Scope::default();
    /// scope.set_workflow_output("login", "token", json!("t1"));
    /// assert_eq!(expr::evaluate("$workflows.login.outputs.token", &scope)?, json!("t1"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_workflow_output(&mut self, workflow: &str, name: &str, v: Value) {
        self.workflows.entry(workflow.into()).or_default().outputs.insert(name.into(), v);
    }
}

/// Tells whether a string is meant as a runtime expression.
///
/// `"$5 off"` and a JSONPath `"$.a"` are literals.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::looks_like_expression;
///
/// assert!(looks_like_expression("$inputs.id"));
/// assert!(looks_like_expression("$response.body#/id"));
/// assert!(!looks_like_expression("$5 off"));
/// assert!(!looks_like_expression("$.a"));
/// ```
pub fn looks_like_expression(s: &str) -> bool {
    ROOTS.iter().any(|r| s.strip_prefix(r).is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '#', '[', ' '])))
}

/// Parses a whole string as one expression.
///
/// # Errors
///
/// The text is not a valid expression; the message names it, e.g.
/// `$steps.a: expected $steps.<id>.outputs.<name>`.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::{self, Root};
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let e = expr::parse("$inputs.user#/name")?;
/// assert!(matches!(e.root, Root::Inputs(ref name) if name == "user"));
/// assert_eq!(e.pointer.as_deref(), Some("/name"));
/// assert!(expr::parse("$steps.a").is_err());
/// # Ok(())
/// # }
/// ```
pub fn parse(text: &str) -> Result<Expr> {
    let (e, rest) = parse_prefix(text)?;
    if !rest.is_empty() {
        return Err(Error(format!("{text}: trailing input after the expression: {rest:?}")));
    }
    Ok(e)
}

/// Parses the expression at the start of `text`.
///
/// For `"$statusCode == 200"` the remainder is `" == 200"`.
///
/// # Errors
///
/// As for [`parse`].
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::{self, Root};
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let (e, rest) = expr::parse_prefix("$statusCode == 200")?;
/// assert_eq!(e.root, Root::StatusCode);
/// assert_eq!(rest, " == 200");
/// # Ok(())
/// # }
/// ```
pub fn parse_prefix(text: &str) -> Result<(Expr, &str)> {
    let fail = |m: &str| Error(format!("{}: {m}", text.split_whitespace().next().unwrap_or(text)));
    let body = text.strip_prefix('$').ok_or_else(|| fail("an expression starts with $"))?;
    let (head, rest) = split_ident(body, false);
    let (root, rest) = parse_root(head, rest).map_err(|m| fail(&m))?;
    let (pointer, rest) = parse_pointer(rest).map_err(|m| fail(&m))?;
    let (path, rest) = accessors(rest).map_err(|m| fail(&m))?;
    Ok((Expr { root, pointer, path }, rest))
}

/// Parses and evaluates one expression string.
///
/// The type is preserved: `$statusCode` gives `200`, `$inputs.user#/tags` gives
/// `["a","b"]`.
///
/// # Errors
///
/// The expression is invalid or references something the scope does not have,
/// e.g. `$inputs.nope: value not set`.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::{self, Scope};
/// use serde_json::json;
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let scope = Scope { inputs: json!({"user": {"tags": ["a", "b"]}}), ..Default::default() };
/// assert_eq!(expr::evaluate("$inputs.user#/tags", &scope)?, json!(["a", "b"]));
/// assert_eq!(expr::evaluate("$inputs.user.tags[1]", &scope)?, json!("b"));
/// assert!(expr::evaluate("$inputs.nope", &scope).is_err());
/// # Ok(())
/// # }
/// ```
pub fn evaluate(text: &str, scope: &Scope) -> Result<Value> {
    eval(&parse(text)?, scope).map_err(|m| Error(format!("{text}: {m}")))
}

/// Evaluates a parsed expression against a scope.
///
/// # Errors
///
/// The reference cannot be resolved in `s`; the message omits the expression
/// text (e.g. `step login has not run yet`) so the caller can prefix it.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::{self, Scope};
/// use serde_json::json;
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let e = expr::parse("$inputs.id")?;
/// let scope = Scope { inputs: json!({"id": 7}), ..Default::default() };
/// assert_eq!(expr::eval(&e, &scope), Ok(json!(7)));
/// # Ok(())
/// # }
/// ```
pub fn eval(e: &Expr, s: &Scope) -> std::result::Result<Value, String> {
    finish(root_value(&e.root, s)?, e)
}

/// Renders a value as embedded into a string (5.9): strings as-is, other
/// scalars as their text, structures as JSON.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::embed;
/// use serde_json::json;
///
/// assert_eq!(embed(&json!("text")), "text");
/// assert_eq!(embed(&json!(200)), "200");
/// assert_eq!(embed(&json!(["a", "b"])), r#"["a","b"]"#);
/// ```
pub fn embed(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Resolves a string value: a whole expression keeps its type, `{$…}`
/// parts are embedded, anything else is a literal.
///
/// `"$inputs.tags"` gives the array itself, `"id={$inputs.id}"` gives `"id=7"`,
/// `"plain"` stays a string.
///
/// # Errors
///
/// An embedded expression is invalid or unresolved.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::{self, Scope};
/// use serde_json::json;
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let scope = Scope { inputs: json!({"id": 7, "tags": ["a"]}), ..Default::default() };
/// assert_eq!(expr::interpolate("$inputs.tags", &scope)?, json!(["a"]));
/// assert_eq!(expr::interpolate("id={$inputs.id}", &scope)?, json!("id=7"));
/// assert_eq!(expr::interpolate("plain", &scope)?, json!("plain"));
/// # Ok(())
/// # }
/// ```
pub fn interpolate(text: &str, scope: &Scope) -> Result<Value> {
    if looks_like_expression(text) {
        return evaluate(text, scope);
    }
    if !text.contains('{') && !text.contains('}') {
        return Ok(Value::String(text.into()));
    }
    let out = embed_fragments(text, scope)?;
    // Empty {} in text is an author error; in a JSON/XML template it is an empty object.
    if text.contains("{}") && !text.trim_start().starts_with(['{', '[', '<']) {
        return Err(Error(format!("{text}: empty {{}}")));
    }
    Ok(Value::String(out))
}

// ---------------------------------------------------------------- parsing

/// Roots that make a `$…` string an expression rather than a literal.
const ROOTS: &[&str] = &[
    "$url", "$method", "$statusCode", "$self", "$request", "$response", "$message", "$inputs", "$outputs",
    "$steps", "$workflows", "$sourceDescriptions", "$components",
];

/// One parse step: the value and the unparsed remainder, or a message.
type Parsed<'a, T> = std::result::Result<(T, &'a str), String>;

/// What follows `$<head>`, as a [`Root`].
fn parse_root<'a>(head: &str, rest: &'a str) -> Parsed<'a, Root> {
    match head {
        "url" => Ok((Root::Url, rest)),
        "method" => Ok((Root::Method, rest)),
        "statusCode" => Ok((Root::StatusCode, rest)),
        "self" => Ok((Root::SelfUri, rest)),
        "request" | "response" | "message" => message_root(head, rest),
        "inputs" | "outputs" => named_root(head, rest),
        "steps" => steps_root(rest),
        "workflows" => workflows_root(rest),
        "sourceDescriptions" => source_root(rest),
        "components" => components_root(rest),
        _ => Err("unknown expression root".into()),
    }
}

/// `$request|$response|$message` + `.header.<n>`, `.body` etc.
fn message_root<'a>(head: &str, rest: &'a str) -> Parsed<'a, Root> {
    let (part, rest) = part(rest, head).ok_or("expected .header.<name>, .query.<name>, .path.<name>, .body or .payload")?;
    let root = match head {
        "request" => Root::Request(part),
        "response" => Root::Response(part),
        _ => Root::Message(part),
    };
    Ok((root, rest))
}

/// `$inputs.<name>` / `$outputs.<name>`; the name may contain dots.
fn named_root<'a>(head: &str, rest: &'a str) -> Parsed<'a, Root> {
    let (name, rest) = split_ident(rest.strip_prefix('.').ok_or("expected a name")?, true);
    if name.is_empty() {
        return Err("empty name".into());
    }
    let root = if head == "inputs" { Root::Inputs(name.into()) } else { Root::Outputs(name.into()) };
    Ok((root, rest))
}

/// `$steps.<id>.outputs.<name>`.
fn steps_root(rest: &str) -> Parsed<'_, Root> {
    const EXPECTED: &str = "expected $steps.<id>.outputs.<name>";
    let (step, rest) = strict(rest).ok_or(EXPECTED)?;
    let (name, rest) = outputs_name(rest).ok_or(EXPECTED)?;
    Ok((Root::Steps { step: step.into(), name: name.into() }, rest))
}

/// `$workflows.<id>.inputs|outputs.<name>` or `$workflows.<id>.steps.<id>.outputs.<name>`.
fn workflows_root(rest: &str) -> Parsed<'_, Root> {
    const EXPECTED: &str = "expected $workflows.<id>.inputs.<name>, .outputs.<name> or .steps.<id>.outputs.<name>";
    let (workflow, rest) = strict(rest).ok_or("expected $workflows.<id>.inputs|outputs.<name>")?;
    let workflow = workflow.to_string();
    if let Some(r) = rest.strip_prefix(".steps.") {
        let (step, r) = split_ident(r, false);
        let (name, r) = outputs_name(r).filter(|_| !step.is_empty()).ok_or(EXPECTED)?;
        return Ok((Root::WorkflowStep { workflow, step: step.into(), name: name.into() }, r));
    }
    let (field, r) = split_ident(rest.strip_prefix('.').ok_or(EXPECTED)?, false);
    let (name, r) = split_ident(r.strip_prefix('.').ok_or(EXPECTED)?, true);
    if name.is_empty() {
        return Err(EXPECTED.into());
    }
    let name = name.to_string();
    match field {
        "inputs" => Ok((Root::WorkflowInputs { workflow, name }, r)),
        "outputs" => Ok((Root::WorkflowOutputs { workflow, name }, r)),
        _ => Err(EXPECTED.into()),
    }
}

/// `$sourceDescriptions.<name>.<reference>`; the reference (operationId,
/// workflowId or field) runs to a delimiter.
fn source_root(rest: &str) -> Parsed<'_, Root> {
    const EXPECTED: &str = "expected $sourceDescriptions.<name>.<reference>";
    let (source, rest) = strict(rest).ok_or(EXPECTED)?;
    let r = rest.strip_prefix('.').ok_or(EXPECTED)?;
    let end = r.find(|c: char| c.is_whitespace() || matches!(c, '}' | '#' | ')')).unwrap_or(r.len());
    if end == 0 {
        return Err("empty reference".into());
    }
    Ok((Root::SourceDescriptions { source: source.into(), reference: r[..end].into() }, &r[end..]))
}

/// `$components.<kind>.<name>` for the three reusable kinds.
fn components_root(rest: &str) -> Parsed<'_, Root> {
    let (kind, r) = split_ident(rest.strip_prefix('.').ok_or("expected $components.<kind>.<name>")?, false);
    if !matches!(kind, "parameters" | "successActions" | "failureActions") {
        return Err("component kind must be parameters, successActions or failureActions".into());
    }
    let (name, r) = split_ident(r.strip_prefix('.').ok_or("missing component name")?, true);
    if name.is_empty() {
        return Err("missing component name".into());
    }
    Ok((Root::Components { kind: kind.into(), name: name.into() }, r))
}

/// An optional `#/json/pointer` suffix, validated.
fn parse_pointer(rest: &str) -> Parsed<'_, Option<String>> {
    let Some(r) = rest.strip_prefix('#') else { return Ok((None, rest)) };
    let end = r.find(|c: char| c.is_whitespace() || matches!(c, '}' | ')' | '{')).unwrap_or(r.len());
    let p = &r[..end];
    validate_pointer(p)?;
    Ok((Some(p.to_string()), &r[end..]))
}

/// `.key` and `[n]` accessors; stops at anything else.
fn accessors(mut s: &str) -> Parsed<'_, Vec<Access>> {
    let mut out = Vec::new();
    loop {
        if let Some(r) = s.strip_prefix('.') {
            let (k, r) = split_ident(r, false);
            if k.is_empty() {
                return Err("empty name after .".into());
            }
            out.push(Access::Key(k.into()));
            s = r;
        } else if let Some(r) = s.strip_prefix('[') {
            let (n, r) = r.split_once(']').ok_or("unclosed [")?;
            out.push(Access::Index(n.trim().parse().map_err(|_| format!("index [{n}] is not a number"))?));
            s = r;
        } else {
            return Ok((out, s));
        }
    }
}

/// `[A-Za-z0-9_-]+` (with dots when `dots`): the identifier at the start.
fn split_ident(s: &str, dots: bool) -> (&str, &str) {
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || (dots && c == '.')))
        .unwrap_or(s.len());
    s.split_at(end)
}

/// `.<identifier-strict>` at the start.
fn strict(s: &str) -> Option<(&str, &str)> {
    let (id, rest) = split_ident(s.strip_prefix('.')?, false);
    (!id.is_empty()).then_some((id, rest))
}

/// `.outputs.<name>` at the start, with a non-empty (possibly dotted) name.
fn outputs_name(s: &str) -> Option<(&str, &str)> {
    s.strip_prefix(".outputs.").map(|r| split_ident(r, true)).filter(|(n, _)| !n.is_empty())
}

/// `.header.<n>`, `.query.<n>`, `.path.<n>`, `.body`, `.payload` after a
/// message root; `$message` has no query/path.
fn part<'a>(s: &'a str, root: &str) -> Option<(Part, &'a str)> {
    let s = s.strip_prefix('.')?;
    let (kind, rest) = split_ident(s, false);
    // A header/parameter name runs up to the condition or template delimiter.
    let named = |r: &'a str| {
        let r = r.strip_prefix('.')?;
        let end = r.find(|c: char| c.is_whitespace() || matches!(c, '}' | ')' | '#' | '[' | '(' | '=' | '!' | '<' | '>' | '&' | '|' | '"' | ',')).unwrap_or(r.len());
        (end > 0).then(|| (r[..end].to_string(), &r[end..]))
    };
    match kind {
        "header" => named(rest).map(|(n, r)| (Part::Header(n), r)),
        "query" if root != "message" => named(rest).map(|(n, r)| (Part::Query(n), r)),
        "path" if root != "message" => named(rest).map(|(n, r)| (Part::Path(n), r)),
        "body" => Some((Part::Body, rest)),
        "payload" => Some((Part::Payload, rest)),
        _ => None,
    }
}

/// RFC 6901 syntax: empty or `/…`, `~` only as `~0`/`~1`.
fn validate_pointer(p: &str) -> std::result::Result<(), String> {
    if !p.is_empty() && !p.starts_with('/') {
        return Err(format!("JSON Pointer {p:?} must start with /"));
    }
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return Err(format!("JSON Pointer {p:?}: only 0 or 1 may follow ~"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- evaluation

/// A value, or a message without the expression text.
type Evaluated = std::result::Result<Value, String>;

/// The value a root names, before pointer and accessors.
fn root_value(root: &Root, s: &Scope) -> Evaluated {
    match root {
        Root::Url => Ok(Value::String(exchange(s)?.url.clone())),
        Root::Method => Ok(Value::String(exchange(s)?.method.clone())),
        Root::StatusCode => Ok(Value::from(exchange(s)?.status)),
        Root::SelfUri => Ok(Value::String(s.self_uri.clone().ok_or("document has no $self")?)),
        Root::Request(part) => request_part(exchange(s)?, part),
        Root::Response(part) => response_part(s, part),
        Root::Message(part) => message_part(s, part),
        Root::Inputs(name) => Ok(s.inputs.as_object().and_then(|m| lookup_dotted(m, name)).ok_or("value not set")?),
        Root::Outputs(name) => Ok(lookup_dotted(&s.outputs, name).ok_or("output not set")?),
        Root::Steps { step, name } => {
            let outs = s.steps.get(step).ok_or(format!("step {step} has not run yet"))?;
            lookup_dotted(outs, name).ok_or(format!("step {step} produced no output {name}"))
        }
        Root::WorkflowInputs { workflow, name } => {
            lookup_dotted(&workflow_state(s, workflow)?.inputs, name).ok_or(format!("workflow {workflow} has no {name}"))
        }
        Root::WorkflowOutputs { workflow, name } => {
            lookup_dotted(&workflow_state(s, workflow)?.outputs, name).ok_or(format!("workflow {workflow} has no {name}"))
        }
        Root::WorkflowStep { workflow, step, name } => {
            let outs = workflow_state(s, workflow)?.steps.get(step).ok_or(format!("step {workflow}/{step} has not run yet"))?;
            lookup_dotted(outs, name).ok_or(format!("step {step} produced no output {name}"))
        }
        Root::SourceDescriptions { source, reference } => source_reference(s, source, reference),
        Root::Components { kind, name } => s
            .components
            .as_ref()
            .and_then(|c| c.get(kind))
            .and_then(|m| m.get(name))
            .cloned()
            .ok_or(format!("no component {kind}.{name}")),
    }
}

fn exchange(s: &Scope) -> std::result::Result<&Exchange, &'static str> {
    s.exchange.as_ref().ok_or("no HTTP exchange at this point")
}

fn workflow_state<'s>(s: &'s Scope, workflow: &str) -> std::result::Result<&'s WorkflowState, String> {
    s.workflows.get(workflow).ok_or(format!("workflow {workflow} has not run yet"))
}

fn request_part(x: &Exchange, part: &Part) -> Evaluated {
    match part {
        Part::Header(n) => header(&x.request_headers, n).map(|v| Value::String(v.into())).ok_or(format!("no header {n}")),
        Part::Query(n) => pair(&x.request_query, n).ok_or(format!("no query parameter {n}")),
        Part::Path(n) => pair(&x.request_path, n).ok_or(format!("no path parameter {n}")),
        Part::Body | Part::Payload => Ok(x.request_body.clone().ok_or("request has no body")?),
    }
}

/// `$response.body` prefers an opaque call's body over the HTTP exchange.
fn response_part(s: &Scope, part: &Part) -> Evaluated {
    match part {
        Part::Header(n) => header(&exchange(s)?.response_headers, n).map(|v| Value::String(v.into())).ok_or(format!("no header {n}")),
        Part::Body | Part::Payload => match (&s.exchange, &s.body) {
            (_, Some(b)) => Ok(b.clone()),
            (Some(x), None) => Ok(x.response_body.clone()),
            (None, None) => Err("no response at this point".into()),
        },
        _ => Err("a response has no query/path".into()),
    }
}

fn message_part(s: &Scope, part: &Part) -> Evaluated {
    let m = s.message.as_ref().ok_or("no message at this point")?;
    match part {
        Part::Header(n) => m.header(n).map(|v| Value::String(v.into())).ok_or(format!("no header {n}")),
        _ => Ok(m.payload.clone()),
    }
}

/// 5.9.2: operationId (or workflowId for arazzo sources) first, then a
/// field of the Source Description Object.
fn source_reference(s: &Scope, source: &str, reference: &str) -> Evaluated {
    let info = s.sources.get(source).ok_or(format!("no source {source}"))?;
    if let Some(api) = &info.api {
        if let Some(op) = crate::openapi::find_operation(api, reference) {
            return Ok(op.op.clone());
        }
        if let Some(op) = api["operations"].get(reference) {
            return Ok(op.clone());
        }
    }
    if let Some(doc) = &info.arazzo {
        if let Some(w) = doc.workflow(reference) {
            return Ok(serde_json::to_value(w).unwrap());
        }
    }
    info.fields.get(reference).cloned().ok_or(format!("source {source} has no {reference}"))
}

fn pair(list: &[(String, String)], name: &str) -> Option<Value> {
    list.iter().find(|(k, _)| k == name).map(|(_, v)| Value::String(v.clone()))
}

/// A dotted name against an object: the longest existing key wins, the
/// rest are property accesses (`$inputs.a.b` → key `"a.b"` if present,
/// else `a` → `b`).
fn lookup_dotted(root: &Map<String, Value>, name: &str) -> Option<Value> {
    let parts: Vec<&str> = name.split('.').collect();
    for take in (1..=parts.len()).rev() {
        if let Some(mut v) = root.get(&parts[..take].join(".")) {
            for p in &parts[take..] {
                v = v.get(*p)?;
            }
            return Some(v.clone());
        }
    }
    None
}

/// Applies the pointer and accessors to a base value.
fn finish(base: Value, e: &Expr) -> Evaluated {
    let mut v = match &e.pointer {
        Some(p) => base.pointer(p).cloned().ok_or(format!("no {p} in the value"))?,
        None => base,
    };
    for a in &e.path {
        v = match a {
            Access::Key(k) => v.get(k).cloned().ok_or(format!("no field {k}"))?,
            Access::Index(i) => v.get(*i).cloned().ok_or(format!("no element [{i}]"))?,
        };
    }
    Ok(v)
}

/// Replaces every `{$…}` of a template with its embedded value; other
/// braces stay literal.
fn embed_fragments(text: &str, scope: &Scope) -> Result<String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        if !after.starts_with('$') {
            // "{" without an expression is a literal (JSON templates: {"a": …})
            out.push_str(&rest[..=open]);
            rest = after;
            continue;
        }
        let (e, tail) = parse_prefix(after)?;
        let tail = tail.strip_prefix('}').ok_or_else(|| Error(format!("{text}: unclosed {{ in expression")))?;
        out.push_str(&rest[..open]);
        out.push_str(&embed(&eval(&e, scope).map_err(|m| Error(format!("{text}: {m}")))?));
        rest = tail;
    }
    out.push_str(rest);
    Ok(out)
}
