//! The built-in HTTP transport (feature `http`) against a real socket:
//! one OpenAPI workflow end to end, no mocks between the engine and TCP.
#![cfg(feature = "http")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

use arazzo_jrpc::transport::http::ReqwestHttp;
use arazzo_jrpc::{Description, Engine, Sources};
use serde_json::json;

/// Answers every request with the path it saw, as JSON.
fn serve() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut w = s.try_clone().unwrap();
            let mut r = BufReader::new(s);
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut len = 0;
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                if h == "\r\n" || h.is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let out = json!({"path": path, "body": String::from_utf8_lossy(&body)}).to_string();
            write!(w, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Served: yes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len()).unwrap();
        }
    });
    format!("http://{addr}")
}

#[test]
fn openapi_workflow_over_real_http() {
    let base = serve();
    let api = json!({
        "openapi": "3.1.0", "info": {"title": "t", "version": "1"},
        "servers": [{"url": format!("{base}/v1")}],
        "paths": {"/pets/{id}": {"post": {"operationId": "touch"}}}
    });
    let mut s = Sources::new();
    s.insert(
        "https://x/flow.arazzo.yaml",
        "arazzo: 1.1.0\ninfo: { title: t, version: '1' }\n\
         sourceDescriptions: [ { name: api, url: api.json, type: openapi } ]\n\
         workflows:\n  - workflowId: w\n    steps:\n      - stepId: s\n        operationId: touch\n\
         \x20       parameters: [ { name: id, in: path, value: 7 }, { name: q, in: query, value: 1 } ]\n\
         \x20       requestBody: { contentType: application/json, payload: { a: 1 } }\n\
         \x20       successCriteria: [ { condition: $statusCode == 200 }, { condition: $response.header.x-served == 'yes' } ]\n\
         \x20       outputs: { path: $response.body#/path, body: $response.body#/body }\n",
    )
    .unwrap();
    s.insert("https://x/api.json", &api.to_string()).unwrap();
    let d = Description::load(&s, "https://x/flow.arazzo.yaml").unwrap();
    let mut http = ReqwestHttp::default();
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(out.steps[0].outputs["path"], "/v1/pets/7?q=1");
    assert_eq!(out.steps[0].outputs["body"], r#"{"a":1}"#);
}
