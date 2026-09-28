//! Conformance against the OAI Arazzo 1.1 test suite (tests/oai, taken
//! from OAI/Arazzo-Specification branch v1.1-dev, schema 2026-04-15).
//!
//! Structural level = what the official JSON Schema accepts: every `pass`
//! document parses, every `fail` document is rejected. Semantic level =
//! what the schema cannot see (source types, resolvable references): the
//! `characterization-*` documents pass the schema but are semantically wrong.

use std::path::Path;

use arazzo_jrpc::Document;

fn files(dir: &str) -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oai").join(dir);
    let mut out: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("yaml")))
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(&p).unwrap()))
        .collect();
    out.sort();
    out
}

#[test]
fn every_official_pass_document_parses() {
    let docs = files("pass");
    assert_eq!(docs.len(), 35, "OAI pass fixture set");
    let failed: Vec<String> = docs
        .iter()
        .filter_map(|(name, text)| Document::parse(text).err().map(|e| format!("{name}: {e}")))
        .collect();
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn every_official_fail_document_is_rejected() {
    let docs = files("fail");
    assert_eq!(docs.len(), 27, "OAI fail fixture set");
    let accepted: Vec<&String> = docs
        .iter()
        .filter(|(_, text)| Document::parse(text).is_ok())
        .map(|(name, _)| name)
        .collect();
    assert!(accepted.is_empty(), "accepted: {accepted:#?}");
}

/// Each fail document is rejected for its own reason, not by accident.
#[test]
fn fail_documents_are_rejected_for_the_right_reason() {
    let expect = [
        ("async-both-operationId-and-channelPath.arazzo.yaml", "channelPath"),
        ("async-channelPath-without-action.arazzo.yaml", "action"),
        ("async-depends-on-duplicate-values.arazzo.yaml", "dependsOn"),
        ("async-depends-on-empty.arazzo.yaml", "dependsOn"),
        ("async-depends-on-invalid-reference.arazzo.yaml", "dependsOn"),
        ("channel-path-is-invalid-for-openapi.arazzo.yaml", "action"),
        ("invalid-arazzo-version.yaml", "arazzo"),
        ("invalid-async-correlation-id-non-receive-action.arazzo.yaml", "correlationId"),
        ("invalid-async-parameter-without-in.arazzo.yaml", "in"),
        ("invalid-async.arazzo.yaml", "action"),
        ("invalid-expression-type-object.arazzo.yaml", "context"),
        ("invalid-openapi-parameter-without-in.arazzo.yaml", "in"),
        ("invalid-openapi-success-criteria.arazzo.yaml", "successCriteria"),
        ("invalid-querystring-without-value.arazzo.yaml", "value"),
        ("invalid-selector-object.arazzo.yaml", "type"),
        ("invalid-self-fragment.arazzo.yaml", "$self"),
        ("invalid-targetSelectorType.arazzo.yaml", "targetSelectorType"),
        ("invalid-workflow-step-with-operationId.arazzo.yaml", "workflowId"),
        ("invalid-workflow-success-criteria.arazzo.yaml", "successCriteria"),
        ("not-an-object.yaml", "object"),
        ("openapi-both-operationId-and-operationPath.arazzo.yaml", "operationPath"),
        ("openapi-duplicate-parameters.arazzo.yaml", "parameters"),
        ("openapi-missing-operation-target.arazzo.yaml", "operationId"),
        ("workflow-duplicate-parameters.arazzo.yaml", "parameters"),
        ("workflow-missing-workflowId.arazzo.yaml", "workflowId"),
        ("workflow-step-with-action.arazzo.yaml", "action"),
        ("workflow-step-with-channelPath.arazzo.yaml", "channelPath"),
    ];
    let docs = files("fail");
    for (name, needle) in expect {
        let text = &docs.iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no {name}")).1;
        let e = Document::parse(text).unwrap_err().to_string();
        assert!(e.contains(needle), "{name}: expected {needle:?} in {e:?}");
    }
}

/// Characterization documents pass the schema; the semantic check, which
/// knows each source's `type`, catches what the schema cannot.
#[test]
fn characterization_documents_fail_the_semantic_check() {
    let expect = [
        ("characterization-asyncapi-operationId-without-action.arazzo.yaml", "action"),
        ("characterization-openapi-invalid-operationPath-string.arazzo.yaml", "operationPath"),
        ("characterization-openapi-operationId-with-action.arazzo.yaml", "action"),
        ("characterization-openapi-receive-with-correlationId.arazzo.yaml", "correlationId"),
        ("characterization-workflow-step-with-requestBody.arazzo.yaml", "requestBody"),
    ];
    let docs = files("pass");
    for (name, needle) in expect {
        let text = &docs.iter().find(|(n, _)| n == name).unwrap().1;
        let doc = Document::parse(text).unwrap();
        let problems = doc.lint();
        assert!(
            problems.iter().any(|p| p.to_string().contains(needle)),
            "{name}: expected {needle:?} in {problems:?}"
        );
    }
}

/// A bare channelPath is a channel name in the single AsyncAPI source (the
/// suite accepts `channelPath: createUser`), so the nonsense one is caught
/// only against the AsyncAPI document, where no such channel exists.
#[test]
fn bare_channel_path_is_checked_against_the_asyncapi_document() {
    let text = &files("pass").into_iter().find(|(n, _)| n.starts_with("characterization-async-invalid-channelPath")).unwrap().1;
    let mut s = arazzo_jrpc::Sources::new();
    s.insert("https://example.com/flow.arazzo.yaml", text).unwrap();
    s.insert(
        "https://events.com/asyncapi.yaml",
        "asyncapi: 3.0.0\ninfo: { title: e, version: '1' }\nchannels: { orders: { address: orders } }\n",
    )
    .unwrap();
    let d = arazzo_jrpc::Description::load(&s, "https://example.com/flow.arazzo.yaml").unwrap();
    assert!(d.check().iter().any(|p| p.to_string().contains("channelPath")), "{:?}", d.check());
}

#[test]
fn clean_pass_documents_have_no_semantic_problems() {
    for (name, text) in files("pass").iter().filter(|(n, _)| !n.starts_with("characterization-")) {
        let problems = Document::parse(text).unwrap().lint();
        assert!(problems.is_empty(), "{name}: {problems:?}");
    }
}

/// Published 1.0.0 examples still load: 1.0.x documents are accepted.
#[test]
fn official_1_0_examples_parse() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oai/examples/1.0.0");
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if !name.contains("arazzo") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        Document::parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        n += 1;
    }
    assert_eq!(n, 6);
}

#[test]
fn arazzo_version_must_be_1_0_or_1_1() {
    let doc = |v: &str| {
        format!(
            "arazzo: {v}\ninfo: {{ title: t, version: '1' }}\n\
             sourceDescriptions: [ {{ name: a, url: a.yaml, type: openapi }} ]\n\
             workflows: [ {{ workflowId: w, steps: [ {{ stepId: s, operationId: op }} ] }} ]\n"
        )
    };
    for ok in ["1.0.0", "1.0.1", "1.1.0", "1.1.7"] {
        Document::parse(&doc(ok)).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
    for bad in ["1.2.0", "2.0.0", "1.1", "'1'"] {
        assert!(Document::parse(&doc(bad)).is_err(), "{bad}");
    }
}

/// JSON is YAML 1.2: the same document in JSON parses identically.
#[test]
fn json_and_yaml_are_the_same_document() {
    let yaml = files("pass").into_iter().find(|(n, _)| n == "minimal.arazzo.yaml").unwrap().1;
    let value: serde_json::Value = serde_yaml_ng::from_str(&yaml).unwrap();
    let from_yaml = Document::parse(&yaml).unwrap();
    let from_json = Document::parse(&serde_json::to_string(&value).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&from_yaml).unwrap(), serde_json::to_value(&from_json).unwrap());
}
