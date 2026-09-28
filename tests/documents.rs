//! Multi-document descriptions (spec 5.3, 5.5, 5.6, Appendix B): base URI
//! from $self or the retrieval URI, relative source URLs, identity-based
//! lookup by $self, arazzo-type sources, cross-document workflows and
//! component scoping.

use arazzo_jrpc::{Description, Engine, Sources};
use serde_json::json;

mod common;
use common::{json_response, MockHttp};

fn minimal(self_line: &str, source_url: &str) -> String {
    format!(
        "arazzo: 1.1.0\n{self_line}info: {{ title: t, version: '1' }}\n\
         sourceDescriptions: [ {{ name: petstore, url: '{source_url}', type: openapi }} ]\n\
         workflows: [ {{ workflowId: w, steps: [ {{ stepId: s, operationId: findPets }} ] }} ]\n"
    )
}

/// Appendix B.1: $self is the base, whatever the retrieval URI.
#[test]
fn base_uri_from_self() {
    let d = Description::parse_at(
        "file:///Users/dev/projects/workflows/purchase.arazzo.yaml",
        &minimal("$self: https://api.example.com/workflows/purchase.arazzo.yaml\n", "../specs/petstore.yaml"),
    )
    .unwrap();
    assert_eq!(d.source_uri("petstore").unwrap(), "https://api.example.com/specs/petstore.yaml");
}

/// Appendix B.2: without $self the retrieval URI is the base.
#[test]
fn base_uri_from_retrieval_uri() {
    let d = Description::parse_at("file:///Users/dev/projects/workflows/purchase.arazzo.yaml", &minimal("", "../specs/petstore.yaml")).unwrap();
    assert_eq!(d.source_uri("petstore").unwrap(), "file:///Users/dev/projects/specs/petstore.yaml");
}

/// Appendix B.5: a relative $self is resolved first, then used as the base.
#[test]
fn relative_self_is_resolved_first() {
    let d = Description::parse_at(
        "https://api.example.com/v2/api-description.yaml",
        &minimal("$self: workflows/purchase.arazzo.yaml\n", "../specs/petstore.yaml"),
    )
    .unwrap();
    assert_eq!(d.self_uri().unwrap(), "https://api.example.com/v2/workflows/purchase.arazzo.yaml");
    assert_eq!(d.source_uri("petstore").unwrap(), "https://api.example.com/v2/specs/petstore.yaml");
}

/// 5.5.2: an absolute source URL resolves to the document whose $self it
/// is, even when that document was retrieved from somewhere else.
#[test]
fn arazzo_sources_resolve_by_identity() {
    let mut s = Sources::new();
    s.insert(
        "https://example.com/main.arazzo.yaml",
        "arazzo: 1.1.0\ninfo: { title: main, version: '1' }\n\
         sourceDescriptions: [ { name: shared, url: 'https://canonical.example.com/shared.arazzo.yaml', type: arazzo } ]\n\
         workflows: [ { workflowId: w, steps: [ { stepId: s, workflowId: $sourceDescriptions.shared.remote } ] } ]\n",
    )
    .unwrap();
    s.insert(
        "file:///cache/abc123.yaml",
        "arazzo: 1.1.0\n$self: https://canonical.example.com/shared.arazzo.yaml\ninfo: { title: shared, version: '1' }\n\
         sourceDescriptions: [ { name: petstore, url: 'https://example.com/specs/petstore.yaml', type: openapi } ]\n\
         workflows: [ { workflowId: remote, steps: [ { stepId: r, operationId: findPets } ] } ]\n",
    )
    .unwrap();
    s.insert(common::PETSTORE_URL, &common::petstore().to_string()).unwrap();
    let d = Description::load(&s, "https://example.com/main.arazzo.yaml").unwrap();
    assert!(d.check().is_empty(), "{:?}", d.check());

    let mut http = MockHttp::with(vec![json_response(200, json!([]))]);
    let out = Engine::new(&d).with_http(&mut http).run("w", json!({})).unwrap();
    assert_eq!(out.steps.iter().map(|x| x.workflow_id.as_str()).collect::<Vec<_>>(), ["remote", "w"]);
    assert_eq!(http.seen[0].url, "https://api.example.com/v1/pets");
}

/// 5.8.9: components are scoped to their document — a workflow from
/// document B does not see the components of document A.
#[test]
fn components_are_scoped_to_their_document() {
    let mut s = Sources::new();
    s.insert(
        "https://example.com/a.arazzo.yaml",
        "arazzo: 1.1.0\ninfo: { title: a, version: '1' }\n\
         sourceDescriptions: [ { name: b, url: b.arazzo.yaml, type: arazzo } ]\n\
         workflows: [ { workflowId: w, steps: [ { stepId: s, workflowId: $sourceDescriptions.b.remote } ] } ]\n\
         components: { parameters: { p: { name: status, in: query, value: from-a } } }\n",
    )
    .unwrap();
    s.insert(
        "https://example.com/b.arazzo.yaml",
        "arazzo: 1.1.0\ninfo: { title: b, version: '1' }\n\
         sourceDescriptions: [ { name: petstore, url: specs/petstore.yaml, type: openapi } ]\n\
         workflows: [ { workflowId: remote, steps: [ { stepId: r, operationId: findPets, parameters: [ { reference: $components.parameters.p } ] } ] } ]\n",
    )
    .unwrap();
    s.insert(common::PETSTORE_URL, &common::petstore().to_string()).unwrap();
    let e = Description::load(&s, "https://example.com/a.arazzo.yaml").unwrap_err();
    assert!(e.to_string().contains("$components.parameters.p"), "{e}");
}

/// 5.5: a missing document is not an error until something needs it.
#[test]
fn missing_sources_are_reported_not_fatal() {
    let mut s = Sources::new();
    s.insert(common::ENTRY, &minimal("", "../specs/nowhere.yaml")).unwrap();
    let d = Description::load(&s, common::ENTRY).unwrap();
    assert!(d.check().iter().any(|p| p.to_string().contains("nowhere.yaml")));
}

#[test]
fn sources_detect_document_kinds() {
    let mut s = Sources::new();
    s.insert("https://x/a.yaml", "openapi: 3.1.0\ninfo: { title: a, version: '1' }\npaths: {}\n").unwrap();
    s.insert("https://x/b.yaml", "asyncapi: 3.0.0\ninfo: { title: b, version: '1' }\n").unwrap();
    s.insert("https://x/c.yaml", &minimal("", "a.yaml")).unwrap();
    assert!(s.insert("https://x/d.yaml", "just: yaml\n").is_err(), "unknown document kind");
    assert!(s.insert("https://x/e.yaml", ": : :").is_err());
}
