//! XPath over an XML string, in the version the Expression Type Object asks
//! for: 3.1/3.0/2.0 through xee, 1.0 through sxd (spec 5.8.12).

/// The result of an XPath evaluation, reduced to what Arazzo needs.
#[derive(Debug, Clone, PartialEq)]
pub enum XValue {
    Bool(bool),
    Number(f64),
    Text(String),
    /// Nodes (as their string values) or a sequence of several atomics.
    Items(Vec<String>),
}

/// Effective Boolean Value of `expr` over `xml` in `version`.
///
/// # Errors
///
/// Invalid XML, invalid XPath or an evaluation error; the criterion then fails
/// (spec 5.8.11.4.5).
pub fn truthy(xml: &str, expr: &str, version: &str) -> Result<bool, String> {
    if version == "xpath-10" {
        return v1::evaluate(xml, expr).map(|v| v1::boolean(&v));
    }
    v3::truthy(xml, expr)
}

/// Evaluates `expr` over `xml` for a Selector Object.
///
/// # Errors
///
/// As for [`truthy`].
pub fn select(xml: &str, expr: &str, version: &str) -> Result<XValue, String> {
    if version == "xpath-10" {
        return v1::evaluate(xml, expr).map(|v| v1::reduce(&v));
    }
    v3::select(xml, expr)
}

/// XPath 3.1 (also serving 3.0 and 2.0, which it extends) via xee.
mod v3 {
    use xee_xpath::{Documents, Queries, Query};

    use super::XValue;

    /// Parses `xml`, compiles `expr` and hands the result sequence to `f`.
    fn run<T>(xml: &str, expr: &str, f: impl FnOnce(&mut Documents, xee_xpath::Sequence) -> Result<T, String>) -> Result<T, String> {
        let mut documents = Documents::new();
        let handle = documents.add_string_without_uri(xml).map_err(|e| format!("XML: {e}"))?;
        let queries = Queries::default();
        let q = queries.sequence(expr).map_err(|e| format!("XPath {expr:?}: {e}"))?;
        let seq = q.execute(&mut documents, handle).map_err(|e| format!("XPath {expr:?}: {e}"))?;
        f(&mut documents, seq)
    }

    /// Effective Boolean Value of the result sequence.
    pub fn truthy(xml: &str, expr: &str) -> Result<bool, String> {
        run(xml, expr, |_, seq| seq.effective_boolean_value().map_err(|e| format!("XPath {expr:?}: {e}")))
    }

    /// One numeric/boolean atomic → `Number`/`Bool`, one other item → `Text`, else `Items`.
    pub fn select(xml: &str, expr: &str) -> Result<XValue, String> {
        run(xml, expr, |documents, seq| {
            let items: Vec<xee_xpath::Item> = seq.iter().collect();
            if let [single] = items.as_slice() {
                if let Ok(atomic) = single.to_atomic() {
                    if atomic.is_numeric() {
                        return f64::try_from(atomic).map(XValue::Number).map_err(|e| e.to_string());
                    }
                    if let Ok(b) = bool::try_from(atomic.clone()) {
                        return Ok(XValue::Bool(b));
                    }
                }
                return single.string_value(documents.xot()).map(XValue::Text).map_err(|e| e.to_string());
            }
            items
                .iter()
                .map(|i| i.string_value(documents.xot()).map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()
                .map(XValue::Items)
        })
    }
}

/// XPath 1.0 via sxd, with 1.0 boolean conversion (section 4.2).
mod v1 {
    use sxd_xpath::Value;

    use super::XValue;

    /// Evaluates `expr` over `xml`; a node-set becomes `Items` in document order.
    pub fn evaluate(xml: &str, expr: &str) -> Result<XValue, String> {
        let package = sxd_document::parser::parse(xml).map_err(|e| format!("XML: {e}"))?;
        let doc = package.as_document();
        let v = sxd_xpath::evaluate_xpath(&doc, expr).map_err(|e| format!("XPath 1.0 {expr:?}: {e}"))?;
        Ok(match v {
            Value::Boolean(b) => XValue::Bool(b),
            Value::Number(n) => XValue::Number(n),
            Value::String(s) => XValue::Text(s),
            Value::Nodeset(ns) => XValue::Items(ns.document_order().iter().map(sxd_xpath::nodeset::Node::string_value).collect()),
        })
    }

    /// 1.0 `boolean()`: non-empty node-set, non-zero non-NaN number,
    /// non-empty string.
    pub fn boolean(v: &XValue) -> bool {
        match v {
            XValue::Bool(b) => *b,
            XValue::Number(n) => *n != 0.0 && !n.is_nan(),
            XValue::Text(s) => !s.is_empty(),
            XValue::Items(i) => !i.is_empty(),
        }
    }

    /// A single node becomes its text, like the 3.1 path.
    pub fn reduce(v: &XValue) -> XValue {
        match v {
            XValue::Items(i) if i.len() == 1 => XValue::Text(i[0].clone()),
            other => other.clone(),
        }
    }
}
