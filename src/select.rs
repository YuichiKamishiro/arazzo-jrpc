//! Selector Object evaluation (spec 5.8.13): JSONPath, XPath or JSON
//! Pointer over a runtime-expression context.
//!
//! Result shape: one node → its value, several → an array, none → `null`.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::expr::{self, Scope};
use crate::model::{Selector, SelectorKind};

/// Applies a Selector Object.
///
/// One match gives the value (`"neo@x"` for `$.profile.email`), several give an
/// array (`["A","B"]` for `$[*].sku`), none gives `null`.
///
/// # Errors
///
/// The context does not evaluate or the selector is invalid for its type.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::expr::Scope;
/// use arazzo_jrpc::model::Selector;
/// use arazzo_jrpc::select;
/// use serde_json::{json, Value};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let scope = Scope { inputs: json!({"list": [{"sku": "A"}, {"sku": "B"}]}), ..Default::default() };
/// let one: Selector = serde_json::from_value(json!({"context": "$inputs.list", "selector": "/0/sku", "type": "jsonpointer"}))?;
/// assert_eq!(select::select(&one, &scope)?, json!("A"));
/// let many: Selector = serde_json::from_value(json!({"context": "$inputs.list", "selector": "$[*].sku", "type": "jsonpath"}))?;
/// assert_eq!(select::select(&many, &scope)?, json!(["A", "B"]));
/// let none: Selector = serde_json::from_value(json!({"context": "$inputs.list", "selector": "$[?@.sku == 'Z']", "type": "jsonpath"}))?;
/// assert_eq!(select::select(&none, &scope)?, Value::Null);
/// # Ok(())
/// # }
/// ```
pub fn select(s: &Selector, scope: &Scope) -> Result<Value> {
    let target = expr::evaluate(&s.context, scope)?;
    match s.kind.kind {
        SelectorKind::Jsonpath => {
            let mut nodes = crate::condition::jsonpath(&s.selector, &target)?;
            Ok(match nodes.len() {
                0 => Value::Null,
                1 => nodes.remove(0),
                _ => Value::Array(nodes),
            })
        }
        SelectorKind::Jsonpointer => target
            .pointer(&s.selector)
            .cloned()
            .ok_or_else(|| Error(format!("JSON Pointer {}: no such path", s.selector))),
        SelectorKind::Xpath => xpath(&target, &s.selector, s.kind.version()),
    }
}

/// An XPath selector over an XML string, reduced to JSON like JSONPath:
/// one node → its text, several → an array of texts, none → `null`.
#[cfg(feature = "xpath")]
fn xpath(target: &Value, selector: &str, version: &str) -> Result<Value> {
    use crate::xpath::XValue;
    let xml = target.as_str().ok_or("XPath applies only to XML (a string)")?;
    Ok(match crate::xpath::select(xml, selector, version).map_err(Error)? {
        XValue::Bool(b) => Value::Bool(b),
        XValue::Number(n) => serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null),
        XValue::Text(t) => Value::String(t),
        XValue::Items(items) => match items.len() {
            0 => Value::Null,
            1 => Value::String(items[0].clone()),
            _ => Value::Array(items.into_iter().map(Value::String).collect()),
        },
    })
}

#[cfg(not(feature = "xpath"))]
fn xpath(_: &Value, _: &str, _: &str) -> Result<Value> {
    Err(Error("XPath unavailable: crate built without the xpath feature".into()))
}
