//! Request bodies and parameter values (spec 5.8.14, 5.8.15): runtime
//! expressions and Selector Objects anywhere inside a structure, templated
//! strings, payload replacements, and encoding into bytes.

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::expr::{self, Root, Scope};
use crate::model::{PayloadReplacement, RequestBody, Selector, SelectorKind, ValueOrSelector};
use crate::select;

/// Everything but RFC 3986 unreserved characters is percent-encoded.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// Percent-encodes one URL component (path segment, query name or value).
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::payload::encode_component;
///
/// assert_eq!(encode_component("a b/c"), "a%20b%2Fc");
/// assert_eq!(encode_component("safe-1.0_~"), "safe-1.0_~");
/// ```
pub fn encode_component(s: &str) -> String {
    utf8_percent_encode(s, COMPONENT).to_string()
}

/// Evaluates a parameter value, output or replacement value.
///
/// `None` when the value is a whole `$inputs.x` expression and `x` was not
/// given; the member is then omitted.
///
/// # Errors
///
/// An expression or selector fails to evaluate.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::Scope;
/// use arazzo_jrpc::model::ValueOrSelector;
/// use arazzo_jrpc::payload;
/// use serde_json::json;
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let scope = Scope { inputs: json!({"id": 7}), ..Default::default() };
/// assert_eq!(payload::value(&ValueOrSelector::Value(json!("$inputs.id")), &scope)?, Some(json!(7)));
/// assert_eq!(payload::value(&ValueOrSelector::Value(json!("$inputs.none")), &scope)?, None, "unset input");
/// # Ok(())
/// # }
/// ```
pub fn value(v: &ValueOrSelector, scope: &Scope) -> Result<Option<Value>> {
    match v {
        ValueOrSelector::Selector(s) => select::select(s, scope).map(Some),
        ValueOrSelector::Value(v) => evaluate(v, scope),
    }
}

/// Evaluates a literal that may hold expressions, templates and Selector
/// Objects at any depth.
///
/// Object members whose value is an unset input are left out; `None` when the
/// whole value is one.
///
/// # Errors
///
/// As for [`value`].
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::Scope;
/// use arazzo_jrpc::payload;
/// use serde_json::json;
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let scope = Scope { inputs: json!({"id": 7, "tags": ["x"]}), ..Default::default() };
/// let tree = json!({"petId": "$inputs.id", "tags": "$inputs.tags", "note": "$inputs.none", "name": "pet-{$inputs.id}"});
/// assert_eq!(payload::evaluate(&tree, &scope)?, Some(json!({"petId": 7, "tags": ["x"], "name": "pet-7"})));
/// # Ok(())
/// # }
/// ```
pub fn evaluate(v: &Value, scope: &Scope) -> Result<Option<Value>> {
    Ok(Some(match v {
        Value::String(s) if unset_input(s, scope) => return Ok(None),
        Value::String(s) => expr::interpolate(s, scope)?,
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|i| evaluate(i, scope).map(Option::unwrap_or_default))
                .collect::<Result<_>>()?,
        ),
        Value::Object(m) => match as_selector(m) {
            Some(s) => select::select(&s, scope)?,
            None => evaluate_members(m, scope)?,
        },
        other => other.clone(),
    }))
}

/// The payload a step sends: evaluated, then with replacements applied.
///
/// `None` when the body has no payload or it is an unset input.
///
/// # Errors
///
/// An expression, selector or replacement fails.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::Scope;
/// use arazzo_jrpc::model::RequestBody;
/// use arazzo_jrpc::payload;
/// use serde_json::json;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let body: RequestBody = serde_json::from_value(json!({
///     "payload": {"petId": 0, "status": "placed"},
///     "replacements": [{"target": "/petId", "value": "$inputs.id"}]
/// }))?;
/// let scope = Scope { inputs: json!({"id": 7}), ..Default::default() };
/// assert_eq!(payload::request_body(&body, &scope)?, Some(json!({"petId": 7, "status": "placed"})));
/// # Ok(())
/// # }
/// ```
pub fn request_body(body: &RequestBody, scope: &Scope) -> Result<Option<Value>> {
    let Some(payload) = &body.payload else { return Ok(None) };
    let Some(mut payload) = evaluate(payload, scope)? else { return Ok(None) };
    for r in &body.replacements {
        if let Some(v) = value(&r.value, scope)? {
            replace(&mut payload, r, v, body.content_type.as_deref()).map_err(|e| Error(format!("replacement {}: {e}", r.target)))?;
        }
    }
    Ok(Some(payload))
}

/// Serializes a payload for the wire.
///
/// A string is sent as-is with the given type (`text/plain` by default), an
/// object with a form type as `k=v&k2=v2`, anything else as JSON
/// (`application/json` by default).
///
/// # Panics
///
/// Only if `serde_json` cannot serialize a `Value`, which it always can.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::payload::encode;
/// use serde_json::json;
///
/// let (bytes, content_type) = encode(&json!({"a": 1}), None);
/// assert_eq!((bytes.as_slice(), content_type.as_str()), (&br#"{"a":1}"#[..], "application/json"));
///
/// let (bytes, _) = encode(&json!({"q": "a b"}), Some("application/x-www-form-urlencoded"));
/// assert_eq!(bytes, b"q=a%20b");
///
/// let (bytes, content_type) = encode(&json!("<a/>"), Some("application/xml"));
/// assert_eq!((bytes.as_slice(), content_type.as_str()), (&b"<a/>"[..], "application/xml"));
/// ```
pub fn encode(payload: &Value, content_type: Option<&str>) -> (Vec<u8>, String) {
    match payload {
        Value::String(s) => (s.clone().into_bytes(), content_type.unwrap_or("text/plain").to_string()),
        Value::Object(m) if content_type.is_some_and(|c| c.starts_with("application/x-www-form-urlencoded")) => {
            let pairs: Vec<String> = m.iter().map(|(k, v)| format!("{}={}", encode_component(k), encode_component(&expr::embed(v)))).collect();
            (pairs.join("&").into_bytes(), content_type.unwrap().to_string())
        }
        other => (serde_json::to_vec(other).expect("Value always serializes"), content_type.unwrap_or("application/json").to_string()),
    }
}

/// Whether `text` is a whole `$inputs.<name>` with no such input given.
///
/// For a dotted name: when no prefix of it is an input.
fn unset_input(text: &str, scope: &Scope) -> bool {
    let Ok(e) = expr::parse(text) else { return false };
    let Root::Inputs(name) = &e.root else { return false };
    let parts: Vec<&str> = name.split('.').collect();
    !(1..=parts.len()).any(|k| scope.inputs.get(parts[..k].join(".")).is_some())
}

/// An object shaped like a Selector Object (context + selector + type, only `x-*` besides).
fn as_selector(m: &Map<String, Value>) -> Option<Selector> {
    let shaped = ["context", "selector", "type"].iter().all(|k| m.contains_key(*k))
        && m.keys().all(|k| matches!(k.as_str(), "context" | "selector" | "type") || k.starts_with("x-"))
        && m["context"].as_str().is_some_and(expr::looks_like_expression);
    shaped.then(|| serde_json::from_value(Value::Object(m.clone())).ok()).flatten()
}

/// Evaluates every member of a plain object; unset-input members are dropped.
fn evaluate_members(m: &Map<String, Value>, scope: &Scope) -> Result<Value> {
    let mut out = Map::new();
    for (k, v) in m {
        if let Some(v) = evaluate(v, scope)? {
            out.insert(k.clone(), v);
        }
    }
    Ok(Value::Object(out))
}

/// Applies one Payload Replacement Object; the target type defaults to
/// XPath for XML payloads and JSON Pointer otherwise.
fn replace(payload: &mut Value, r: &PayloadReplacement, v: Value, content_type: Option<&str>) -> Result<()> {
    let xml = payload.is_string() || content_type.is_some_and(|c| c.contains("xml"));
    let kind = r.target_selector_type.as_ref().map(|t| t.kind).unwrap_or(if xml { SelectorKind::Xpath } else { SelectorKind::Jsonpointer });
    match kind {
        SelectorKind::Jsonpointer => set_pointer(payload, &r.target, v),
        SelectorKind::Jsonpath => {
            let path = serde_json_path::JsonPath::parse(&r.target).map_err(|e| Error(format!("JSONPath: {e}")))?;
            let pointers: Vec<String> = path.query_located(payload).locations().map(|l| l.to_json_pointer()).collect();
            pointers.iter().try_for_each(|p| set_pointer(payload, p, v.clone()))
        }
        SelectorKind::Xpath => {
            let xml = payload.as_str().ok_or("XPath replacement applies only to XML (a string)")?;
            *payload = Value::String(xml_replace(xml, &r.target, &expr::embed(&v))?);
            Ok(())
        }
    }
}

/// Sets the value at a JSON Pointer, creating the last member if missing.
fn set_pointer(root: &mut Value, pointer: &str, v: Value) -> Result<()> {
    if pointer.is_empty() {
        *root = v;
        return Ok(());
    }
    let (parent, last) = pointer.rsplit_once('/').ok_or_else(|| Error(format!("JSON Pointer {pointer:?} must start with /")))?;
    let last = crate::openapi::unescape(last);
    match root.pointer_mut(parent) {
        Some(Value::Object(m)) => {
            m.insert(last, v);
        }
        Some(Value::Array(a)) if last == "-" => a.push(v),
        Some(Value::Array(a)) => {
            let slot = last.parse::<usize>().ok().and_then(|i| a.get_mut(i)).ok_or_else(|| Error(format!("{pointer}: no element {last}")))?;
            *slot = v;
        }
        _ => return Err(Error(format!("{pointer}: no object or array at {parent:?}"))),
    }
    Ok(())
}

/// Sets the text of every node the XPath selects.
///
/// The `<?xml?>` declaration is kept only when the input had one.
// ponytail: XPath 1.0 (sxd) whatever the declared version — enough for replacement paths; xee cannot mutate the tree, switch when 2.0+ functions are needed.
#[cfg(feature = "xpath")]
fn xml_replace(xml: &str, xpath: &str, text: &str) -> Result<String> {
    let package = sxd_document::parser::parse(xml).map_err(|e| Error(format!("XML: {e}")))?;
    let doc = package.as_document();
    let sxd_xpath::Value::Nodeset(nodes) = sxd_xpath::evaluate_xpath(&doc, xpath).map_err(|e| Error(format!("XPath {xpath:?}: {e}")))? else {
        return Err(Error(format!("XPath {xpath:?}: must select nodes")));
    };
    for n in nodes.iter() {
        set_text(n, xpath, text)?;
    }
    let mut out = Vec::new();
    sxd_document::writer::format_document(&doc, &mut out).map_err(|e| Error(format!("XML: {e}")))?;
    let out = String::from_utf8(out).map_err(|e| Error(format!("XML: {e}")))?;
    // The writer adds an <?xml?> declaration; keep it only if the input had one.
    Ok(match out.strip_prefix("<?xml version='1.0'?>") {
        Some(rest) if !xml.trim_start().starts_with("<?xml") => rest.to_string(),
        _ => out,
    })
}

/// Replaces the text of one selected node: element content, text node or attribute value.
#[cfg(feature = "xpath")]
fn set_text(node: sxd_xpath::nodeset::Node, xpath: &str, text: &str) -> Result<()> {
    use sxd_xpath::nodeset::Node;
    match node {
        Node::Element(e) => {
            e.set_text(text);
        }
        Node::Text(t) => t.set_text(text),
        Node::Attribute(a) => {
            if let Some(e) = a.parent() {
                e.set_attribute_value(a.name(), text);
            }
        }
        _ => return Err(Error(format!("XPath {xpath:?}: only elements, text and attributes can be replaced"))),
    }
    Ok(())
}

/// Stand-in for builds without the `xpath` feature.
#[cfg(not(feature = "xpath"))]
fn xml_replace(_: &str, _: &str, _: &str) -> Result<String> {
    Err(Error("XPath unavailable: crate built without the xpath feature".into()))
}
