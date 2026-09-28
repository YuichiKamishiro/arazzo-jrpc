//! OpenAPI descriptions: finding the operation a step names, building the
//! HTTP request for it and reading the response back into an [`Exchange`].

use std::time::Duration;

use serde_json::Value;

use crate::expr::embed;
use crate::model::ParameterIn;
use crate::payload::{encode, encode_component};
use crate::transport::{header, Exchange, HttpRequest, HttpResponse};

/// Path Item keys that hold an operation (`query` is OpenAPI 3.2).
const METHODS: &[&str] = &["get", "put", "post", "delete", "options", "head", "patch", "trace", "query"];

/// One operation of an OpenAPI description, with where it lives.
#[derive(Debug, Clone)]
pub struct Operation<'a> {
    /// Upper-case HTTP method.
    pub method: String,
    /// Path template as written under `paths`.
    pub path: String,
    pub op: &'a Value,
    pub path_item: &'a Value,
    pub doc: &'a Value,
}

impl Operation<'_> {
    /// Declared parameters: path-level first, operation-level overriding by
    /// `name` + `in`; `$ref`s into the document are followed.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::openapi;
    /// use serde_json::json;
    ///
    /// let api = json!({"paths": {"/pets/{petId}": {
    ///     "parameters": [{"name": "petId", "in": "path", "required": true}],
    ///     "get": {"operationId": "getPetById", "parameters": [{"name": "petId", "in": "path"}, {"name": "verbose", "in": "query"}]}
    /// }}});
    /// let op = openapi::find_operation(&api, "getPetById").expect("declared");
    /// let names: Vec<&str> = op.parameters().iter().map(|p| p["name"].as_str().unwrap()).collect();
    /// assert_eq!(names, ["petId", "verbose"], "the operation's petId replaced the path item's");
    /// ```
    pub fn parameters(&self) -> Vec<&Value> {
        let mut out: Vec<&Value> = Vec::new();
        for list in [&self.path_item["parameters"], &self.op["parameters"]] {
            for p in list.as_array().into_iter().flatten().map(|p| deref(self.doc, p)) {
                out.retain(|q| !(q["name"] == p["name"] && q["in"] == p["in"]));
                out.push(p);
            }
        }
        out
    }

    /// The server URL for this operation: operation, path item, then
    /// document `servers`, with variables replaced by their defaults.
    ///
    /// Without a trailing `/`; empty when none is declared.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::openapi;
    /// use serde_json::json;
    ///
    /// let api = json!({
    ///     "servers": [{"url": "https://{region}.example.com/v1/", "variables": {"region": {"default": "eu"}}}],
    ///     "paths": {"/pets": {"get": {"operationId": "findPets"}}}
    /// });
    /// let op = openapi::find_operation(&api, "findPets").expect("declared");
    /// assert_eq!(op.server(), "https://eu.example.com/v1");
    /// ```
    pub fn server(&self) -> String {
        let servers = [&self.op["servers"], &self.path_item["servers"], &self.doc["servers"]]
            .into_iter()
            .find_map(|s| s.as_array().and_then(|a| a.first()));
        let Some(server) = servers else { return String::new() };
        let mut url = server["url"].as_str().unwrap_or("").to_string();
        for (name, var) in server["variables"].as_object().into_iter().flatten() {
            if let Some(default) = var["default"].as_str() {
                url = url.replace(&format!("{{{name}}}"), default);
            }
        }
        url.trim_end_matches('/').to_string()
    }
}

/// Finds the operation with this `operationId`.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::openapi;
/// use serde_json::json;
///
/// let api = json!({"paths": {"/pets/{petId}": {"delete": {"operationId": "deletePet"}}}});
/// let op = openapi::find_operation(&api, "deletePet").expect("declared");
/// assert_eq!((op.method.as_str(), op.path.as_str()), ("DELETE", "/pets/{petId}"));
/// assert!(openapi::find_operation(&api, "nope").is_none());
/// ```
pub fn find_operation<'a>(doc: &'a Value, operation_id: &str) -> Option<Operation<'a>> {
    operations(doc).find(|o| o.op["operationId"] == operation_id)
}

/// Finds the operation at a JSON Pointer such as `/paths/~1pets~1{petId}/get`.
///
/// The pointer must be `/paths/<path>/<method>`.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::openapi;
/// use serde_json::json;
///
/// let api = json!({"paths": {"/pets/{petId}": {"get": {"operationId": "getPetById"}}}});
/// let op = openapi::at_pointer(&api, "/paths/~1pets~1{petId}/get").expect("declared");
/// assert_eq!(op.op["operationId"], "getPetById");
/// assert!(openapi::at_pointer(&api, "/paths/~1pets~1{petId}").is_none(), "no method");
/// ```
pub fn at_pointer<'a>(doc: &'a Value, pointer: &str) -> Option<Operation<'a>> {
    let tokens: Vec<String> = pointer.split('/').skip(1).map(unescape).collect();
    let [paths, path, method] = tokens.as_slice() else { return None };
    if paths != "paths" || !METHODS.contains(&method.as_str()) {
        return None;
    }
    let item = doc["paths"].get(path)?;
    Some(Operation {
        method: method.to_ascii_uppercase(),
        path: path.clone(),
        op: item.get(method)?,
        path_item: item,
        doc,
    })
}

/// One evaluated step parameter: name, where it goes (as written) and value.
pub type ParamValue = (String, Option<ParameterIn>, Value);

/// Builds the HTTP request for an operation.
///
/// Path parameters fill `{name}` (percent-encoded), query parameters are
/// appended in order, a `querystring` parameter is the whole query as
/// given, cookies are joined into one `Cookie` header; a parameter without
/// `in` goes where the operation declares it (query if it does not).
///
/// The exchange holds the request side (`$url`, `$method`, `$request.*`) for
/// [`read_response`].
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::model::ParameterIn;
/// use arazzo_jrpc::openapi;
/// use serde_json::json;
///
/// let api = json!({
///     "servers": [{"url": "https://api.example.com/v1"}],
///     "paths": {"/pets/{petId}": {"get": {"operationId": "getPetById"}}}
/// });
/// let op = openapi::find_operation(&api, "getPetById").expect("declared");
/// let params = [
///     ("petId".to_string(), Some(ParameterIn::Path), json!("a b")),
///     ("status".to_string(), Some(ParameterIn::Query), json!("sold")),
/// ];
/// let (request, exchange) = openapi::build_request(&op, &params, None, None, None);
/// assert_eq!(request.url, "https://api.example.com/v1/pets/a%20b?status=sold");
/// assert_eq!((exchange.method.as_str(), exchange.url.as_str()), ("GET", request.url.as_str()));
/// ```
pub fn build_request(op: &Operation, params: &[ParamValue], payload: Option<&Value>, content_type: Option<&str>, timeout: Option<Duration>) -> (HttpRequest, Exchange) {
    let mut x = Exchange::default();
    let (path, query, mut headers) = place_parameters(op, params, &mut x);
    let body = payload.map(|p| {
        let (bytes, ct) = encode(p, content_type);
        headers.push(("Content-Type".into(), ct));
        bytes
    });
    let mut url = format!("{}{path}", op.server());
    if !query.is_empty() {
        url = format!("{url}?{}", query.join("&"));
    }
    let request = HttpRequest { method: op.method.clone(), url, headers, body, timeout };
    x.url = request.url.clone();
    x.method = request.method.clone();
    x.request_headers = request.headers.clone();
    x.request_body = payload.cloned();
    (request, x)
}

/// Completes an exchange with the response.
///
/// The body is parsed as JSON for JSON media types (`application/json`,
/// `*+json`), otherwise kept as a string; an empty body is `null`.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::openapi;
/// use arazzo_jrpc::transport::{Exchange, HttpResponse};
/// use serde_json::{json, Value};
///
/// let json_body = HttpResponse {
///     status: 200,
///     headers: vec![("Content-Type".into(), "application/json".into())],
///     body: br#"{"id": 7}"#.to_vec(),
/// };
/// let x = openapi::read_response(Exchange::default(), &json_body);
/// assert_eq!((x.status, &x.response_body), (200, &json!({"id": 7})));
///
/// let xml_body = HttpResponse { status: 200, headers: vec![], body: b"<a>1</a>".to_vec() };
/// assert_eq!(openapi::read_response(Exchange::default(), &xml_body).response_body, json!("<a>1</a>"));
/// assert_eq!(openapi::read_response(Exchange::default(), &HttpResponse::default()).response_body, Value::Null);
/// ```
pub fn read_response(mut x: Exchange, response: &HttpResponse) -> Exchange {
    x.status = response.status;
    x.response_headers = response.headers.clone();
    x.response_body = match (&response.body[..], is_json(&response.headers)) {
        ([], _) => Value::Null,
        (bytes, true) => serde_json::from_slice(bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into())),
        (bytes, false) => Value::String(String::from_utf8_lossy(bytes).into()),
    };
    x
}

/// RFC 6901 token unescaping: `~1` → `/`, `~0` → `~`.
pub(crate) fn unescape(t: &str) -> String {
    t.replace("~1", "/").replace("~0", "~")
}

/// Walks every operation of the document, path by path, in [`METHODS`] order.
fn operations(doc: &Value) -> impl Iterator<Item = Operation<'_>> {
    doc["paths"].as_object().into_iter().flatten().flat_map(move |(path, item)| {
        METHODS.iter().filter_map(move |m| {
            item.get(*m).map(|op| Operation {
                method: m.to_ascii_uppercase(),
                path: path.clone(),
                op,
                path_item: item,
                doc,
            })
        })
    })
}

/// Follows a local `$ref` (`#/components/parameters/x`) once.
///
/// `v` itself when it is not a resolvable local `$ref`.
fn deref<'a>(doc: &'a Value, v: &'a Value) -> &'a Value {
    v["$ref"]
        .as_str()
        .and_then(|r| r.strip_prefix('#'))
        .and_then(|p| doc.pointer(p))
        .unwrap_or(v)
}

/// Puts each parameter where it goes and records the request side in `x`.
fn place_parameters(op: &Operation, params: &[ParamValue], x: &mut Exchange) -> (String, Vec<String>, Vec<(String, String)>) {
    let mut path = op.path.clone();
    let (mut query, mut headers, mut cookies) = (Vec::new(), Vec::new(), Vec::new());
    for (name, location, v) in params {
        let text = embed(v);
        match location.unwrap_or_else(|| declared_in(op, name)) {
            ParameterIn::Path => {
                path = path.replace(&format!("{{{name}}}"), &encode_component(&text));
                x.request_path.push((name.clone(), text));
            }
            ParameterIn::Query => {
                query.push(format!("{}={}", encode_component(name), encode_component(&text)));
                x.request_query.push((name.clone(), text));
            }
            ParameterIn::Querystring => query.push(text),
            ParameterIn::Header | ParameterIn::Channel => headers.push((name.clone(), text)),
            ParameterIn::Cookie => cookies.push(format!("{name}={text}")),
        }
    }
    if !cookies.is_empty() {
        headers.push(("Cookie".into(), cookies.join("; ")));
    }
    (path, query, headers)
}

/// Where the operation declares a parameter.
///
/// `Query` when the operation does not declare the parameter.
fn declared_in(op: &Operation, name: &str) -> ParameterIn {
    op.parameters()
        .into_iter()
        .find(|p| p["name"] == name)
        .and_then(|p| serde_json::from_value(p["in"].clone()).ok())
        .unwrap_or(ParameterIn::Query)
}

/// Whether the `Content-Type` is a JSON media type (`application/json`, `*+json`).
fn is_json(headers: &[(String, String)]) -> bool {
    let media = header(headers, "Content-Type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    media == "application/json" || media.ends_with("+json")
}
