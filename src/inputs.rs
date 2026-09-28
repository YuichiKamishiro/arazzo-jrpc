//! Workflow inputs: filling in what the caller did not give, then
//! validating against the workflow's JSON Schema 2020-12 (spec 5.8.4),
//! with `$ref` into `components.inputs` (by JSON Pointer and `$anchor`).

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use crate::error::{Error, Problem, Result};
use crate::model::Components;

/// Resolves and validates the inputs of one workflow run.
///
/// # Errors
///
/// The inputs do not match the schema; one line per problem, e.g.
/// `/user/id: 0 is less than the minimum of 1`.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::inputs;
/// use serde_json::json;
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let schema = json!({
///     "type": "object",
///     "required": ["number"],
///     "properties": {"number": {"type": "string"}, "kind": {"type": "string", "default": "msisdn"}}
/// });
/// let given = inputs::resolve(Some(&schema), None, &json!({"number": "7926"}))?;
/// assert_eq!(given["kind"], "msisdn", "the default filled in");
/// assert!(inputs::resolve(Some(&schema), None, &json!({})).is_err(), "number is required");
/// # Ok(())
/// # }
/// ```
pub fn resolve(schema: Option<&Value>, components: Option<&Components>, given: &Value) -> Result<Map<String, Value>> {
    let mut out = given.as_object().cloned().unwrap_or_default();
    let Some(schema) = schema else { return Ok(out) };
    fill(schema, &mut out);
    validate(schema, components, &Value::Object(out.clone()))?;
    Ok(out)
}

/// The declared `properties` of an object schema.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::inputs;
/// use serde_json::json;
///
/// let schema = json!({"type": "object", "properties": {"kind": {"type": "string", "default": "msisdn"}}});
/// let (name, prop) = inputs::properties(&schema).next().expect("one property");
/// assert_eq!((name, &prop["default"]), ("kind", &json!("msisdn")));
/// ```
pub fn properties(schema: &Value) -> impl Iterator<Item = (&str, &Value)> {
    schema.get("properties").and_then(Value::as_object).into_iter().flatten().map(|(k, v)| (k.as_str(), v))
}

/// Names of the declared properties.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::inputs;
/// use serde_json::json;
///
/// let schema = json!({"type": "object", "properties": {"number": {}, "kind": {}}});
/// let names: Vec<String> = inputs::names(&schema).into_iter().collect();
/// assert_eq!(names, ["kind", "number"]);
/// ```
pub fn names(schema: &Value) -> BTreeSet<String> {
    properties(schema).map(|(n, _)| n.to_string()).collect()
}

/// Fills missing top-level properties with their `default`; without a `default` the property stays absent.
fn fill(schema: &Value, out: &mut Map<String, Value>) {
    for (name, prop) in properties(schema) {
        if let (false, Some(v)) = (out.contains_key(name), prop.get("default")) {
            out.insert(name.to_string(), v.clone());
        }
    }
}

/// Validates the filled object against the workflow schema; all violations go into one error.
fn validate(schema: &Value, components: Option<&Components>, instance: &Value) -> Result<()> {
    let root = synthetic_root(schema, components);
    let validator = jsonschema::draft202012::new(&root).map_err(|e| Error(format!("inputs schema is invalid: {e}")))?;
    let problems: Vec<Problem> = validator.iter_errors(instance).map(|e| problem(&e)).collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::from_problems(&problems))
    }
}

/// Key in `$defs` holding the component schemas (does not clash with user names).
const COMPONENTS_DEFS: &str = "arazzo-components";

/// Workflow schema + `components.inputs` (for `$ref` by pointer from the document root)
/// + the same schemas in `$defs` (jsonschema finds `$anchor` only there).
fn synthetic_root(schema: &Value, components: Option<&Components>) -> Value {
    let (Some(obj), Some(c)) = (schema.as_object(), components) else { return schema.clone() };
    let inputs: Map<String, Value> = c.inputs.clone().into_iter().collect();
    let mut root = obj.clone();
    // ponytail: do not overwrite `components` in the workflow schema itself, then pointer $ref goes into it
    root.entry("components").or_insert_with(|| json!({ "inputs": inputs }));
    let defs = root.entry("$defs").or_insert_with(|| json!({}));
    if let Some(defs) = defs.as_object_mut() {
        defs.insert(COMPONENTS_DEFS.into(), json!({ "$defs": inputs }));
    }
    Value::Object(root)
}

/// `"<instance path>: <jsonschema message>"`, with ` (<keyword>)` if the message does not name it.
fn problem(e: &jsonschema::ValidationError) -> Problem {
    let mut message = e.to_string();
    let keyword = e.kind().keyword();
    if !keyword.is_empty() && !message.contains(keyword) {
        message = format!("{message} ({keyword})");
    }
    Problem::new(e.instance_path().to_string(), message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(schema: &Value, given: Value) -> Result<Map<String, Value>> {
        resolve(Some(schema), None, &given)
    }

    fn components(inputs: Value) -> Components {
        Components { inputs: serde_json::from_value(inputs).unwrap(), ..Default::default() }
    }

    fn user_schema() -> Value {
        json!({
            "type": "object", "required": ["user"],
            "properties": {
                "user": { "type": "object", "required": ["id"], "properties": {
                    "id": { "type": "integer", "minimum": 1 },
                    "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 2 } } },
                "mail": { "type": "string", "pattern": "^[^@]+@[^@]+$" } }
        })
    }

    #[test]
    fn given_wins_default_fills_the_rest() {
        let s = json!({ "type": "object", "properties": {
            "g": { "default": "d" },
            "d": { "default": "d" },
            "num": { "type": "integer", "default": 7 },
            "opt": { "type": "string" } } });
        let out = run(&s, json!({"g": "given"})).unwrap();
        assert_eq!(out["g"], "given");
        assert_eq!(out["d"], "d");
        assert_eq!(out["num"], 7);
        assert!(!out.contains_key("opt"), "optional and not given: absent");
    }

    #[test]
    fn x_fields_are_not_interpreted() {
        std::env::set_var("ARAZZO_INPUTS_T_ENV", "from-env");
        let s = json!({ "type": "object", "properties": {
            "e": { "type": "string", "x-env": "ARAZZO_INPUTS_T_ENV", "default": "{root}/d" },
            "x": { "type": "string", "x-env": "ARAZZO_INPUTS_T_ENV", "x-generate": "k" } } });
        let out = run(&s, json!({})).unwrap();
        assert_eq!(out["e"], "{root}/d", "default only, as is");
        assert!(!out.contains_key("x"));
    }

    #[test]
    fn filled_values_are_validated_too() {
        let s = json!({ "type": "object", "properties": { "n": { "type": "integer", "default": "oops" } } });
        let e = run(&s, json!({})).unwrap_err().0;
        assert!(e.contains("/n") && e.contains("type"), "{e}");
    }

    #[test]
    fn nested_errors_name_property_and_keyword() {
        let s = user_schema();
        run(&s, json!({"user": {"id": 1, "tags": ["a"]}, "mail": "a@b"})).unwrap();
        for (bad, needles) in [
            (json!({}), ["user", "required"]),
            (json!({"user": {"id": 0}}), ["/user/id", "minimum"]),
            (json!({"user": {"id": 1, "tags": ["a", "b", "c"]}}), ["/user/tags", "maxItems"]),
            (json!({"user": {"id": 1, "tags": [1]}}), ["/user/tags/0", "type"]),
            (json!({"user": {"id": 1}, "mail": "nope"}), ["/mail", "pattern"]),
        ] {
            let e = run(&s, bad).unwrap_err().0;
            for n in needles {
                assert!(e.contains(n), "{n}: {e}");
            }
        }
    }

    #[test]
    fn every_problem_gets_its_own_line() {
        let e = run(&user_schema(), json!({"user": {"id": 0}, "mail": "nope"})).unwrap_err().0;
        assert_eq!(e.lines().count(), 2, "{e}");
    }

    #[test]
    fn ref_by_pointer_and_anchor() {
        let s = json!({ "type": "object", "properties": {
            "page": { "$ref": "#/components/inputs/pagination" },
            "who": { "$ref": "#person" } } });
        let c = components(json!({
            "pagination": { "type": "object", "properties": { "page": { "type": "integer", "format": "int32" } }, "required": ["page"] },
            "person": { "$anchor": "person", "type": "string", "minLength": 2 } }));
        let run = |i: Value| resolve(Some(&s), Some(&c), &i);
        run(json!({"page": {"page": 1}, "who": "neo"})).unwrap();
        let e = run(json!({"page": {}})).unwrap_err().0;
        assert!(e.contains("/page") && e.contains("page"), "{e}");
        let e = run(json!({"who": "x"})).unwrap_err().0;
        assert!(e.contains("/who") && e.contains("minLength"), "{e}");
    }

    #[test]
    fn ref_to_missing_component_is_a_schema_error() {
        let s = json!({ "properties": { "a": { "$ref": "#/components/inputs/nope" } } });
        let e = resolve(Some(&s), Some(&components(json!({}))), &json!({})).unwrap_err().0;
        assert!(e.contains("inputs schema is invalid"), "{e}");
    }

    #[test]
    fn user_defs_survive_next_to_components() {
        let s = json!({ "$defs": { "id": { "type": "integer" } }, "properties": {
            "a": { "$ref": "#/$defs/id" }, "b": { "$ref": "#p" } } });
        let c = components(json!({ "p": { "$anchor": "p", "type": "string" } }));
        let run = |i: Value| resolve(Some(&s), Some(&c), &i);
        run(json!({"a": 1, "b": "s"})).unwrap();
        assert!(run(json!({"a": "1"})).is_err());
        assert!(run(json!({"b": 1})).is_err());
    }

    #[test]
    fn extensions_and_format_do_not_assert() {
        let s = json!({ "type": "object", "properties": {
            "m": { "type": "string", "format": "email", "x-env": "ARAZZO_INPUTS_T_NONE", "x-generate": "none" } } });
        run(&s, json!({"m": "not an email"})).unwrap();
    }

    #[test]
    fn garbage_schema_is_reported() {
        for s in [json!({"type": 5}), json!({"minimum": "x"}), json!({"properties": {"a": {"$ref": "http://example.com/s.json"}}})] {
            let e = run(&s, json!({})).unwrap_err().0;
            assert!(e.contains("inputs schema is invalid"), "{s}: {e}");
        }
    }

    #[test]
    fn no_schema_returns_given_unchanged() {
        assert_eq!(resolve(None, None, &json!({"a": 1})).unwrap(), json!({"a": 1}).as_object().cloned().unwrap());
        assert!(resolve(None, None, &json!([1])).unwrap().is_empty());
    }

    #[test]
    fn names_lists_declared_properties() {
        assert_eq!(names(&user_schema()), BTreeSet::from(["mail".to_string(), "user".to_string()]));
        assert_eq!(names(&json!(true)).len(), 0);
    }
}
