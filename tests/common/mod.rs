//! Shared test doubles: a scripted HTTP transport, a petstore OpenAPI
//! description and a builder for one-document descriptions.
#![allow(dead_code)]

use std::collections::VecDeque;

use arazzo_jrpc::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};
use arazzo_jrpc::{Description, Sources};
use serde_json::{json, Value};

pub const ENTRY: &str = "https://example.com/flows/main.arazzo.yaml";
pub const PETSTORE_URL: &str = "https://example.com/specs/petstore.yaml";

/// Minimal OpenAPI 3.1 description the tests call into.
pub fn petstore() -> Value {
    json!({
      "openapi": "3.1.0",
      "info": {"title": "petstore", "version": "1"},
      "servers": [{"url": "https://api.example.com/v1"}],
      "paths": {
        "/login": {"post": {"operationId": "loginUser"}},
        "/pets": {
          "get": {"operationId": "findPets", "parameters": [
            {"name": "status", "in": "query"}, {"name": "X-Trace", "in": "header"}]},
          "post": {"operationId": "addPet"}
        },
        "/pets/{petId}": {
          "get": {"operationId": "getPetById", "parameters": [{"name": "petId", "in": "path", "required": true}]},
          "delete": {"operationId": "deletePet"}
        },
        "/search": {"get": {"operationId": "search", "parameters": [
            {"name": "q", "in": "querystring", "content": {"application/x-www-form-urlencoded": {}}}]}},
        "/session": {"get": {"operationId": "whoAmI", "parameters": [{"name": "sid", "in": "cookie"}]}}
      }
    })
}

/// A description whose entry document is `arazzo` (YAML) and whose single
/// source `petstore` is [`petstore`].
pub fn describe(arazzo: &str) -> Description {
    try_describe(arazzo).unwrap()
}

pub fn try_describe(arazzo: &str) -> arazzo_jrpc::Result<Description> {
    let mut s = Sources::new();
    s.insert(ENTRY, arazzo)?;
    s.insert(PETSTORE_URL, &serde_json::to_string(&petstore()).unwrap())?;
    Description::load(&s, ENTRY)
}

/// Header shared by most execution tests; `$workflows` goes after it.
pub const HEADER: &str = "\
arazzo: 1.1.0
info: { title: t, version: '1' }
sourceDescriptions:
  - { name: petstore, url: ../specs/petstore.yaml, type: openapi }
";

/// Scripted HTTP: answers in order, records every request.
#[derive(Default)]
pub struct MockHttp {
    pub answers: VecDeque<HttpResponse>,
    pub seen: Vec<HttpRequest>,
}

impl MockHttp {
    pub fn with(answers: Vec<HttpResponse>) -> Self {
        MockHttp { answers: answers.into(), seen: vec![] }
    }
}

impl HttpTransport for MockHttp {
    fn send(&mut self, req: &HttpRequest) -> Result<HttpResponse, TransportError> {
        self.seen.push(req.clone());
        self.answers
            .pop_front()
            .ok_or_else(|| TransportError::new("mock: out of responses"))
    }
}

pub fn json_response(status: u16, body: Value) -> HttpResponse {
    HttpResponse {
        status,
        headers: vec![("Content-Type".into(), "application/json".into())],
        body: serde_json::to_vec(&body).unwrap(),
    }
}

pub fn response_with_headers(status: u16, headers: &[(&str, &str)], body: Value) -> HttpResponse {
    let mut r = json_response(status, body);
    r.headers.extend(headers.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    r
}
