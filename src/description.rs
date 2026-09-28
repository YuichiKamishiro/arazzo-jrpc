//! An Arazzo Description: the entry document plus every document it reaches
//! through `sourceDescriptions`, with each source URL resolved to a
//! document by identity or location (spec 5.3, 5.5, 5.6).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::Value;
use url::Url;

use crate::document::{split_source_path, SOURCE_PREFIX};
use crate::error::{Error, Problem, Result};
use crate::model::{ActionOrRef, AsyncAction, Document, Step, StepTarget, Workflow};
use crate::sources::{absolute, SourceDoc, Sources};
use crate::{asyncapi, openapi, refs, schema};

/// What a source description turned out to point at.
#[derive(Debug, Clone)]
pub enum Target {
    /// Another Arazzo document: index into [`Description::documents`].
    Arazzo(usize),
    OpenApi(Arc<Value>),
    AsyncApi(Arc<Value>),
    /// No `type` and no document: served by a caller's `Invoker`.
    Opaque,
    /// Typed, but no document at that URI was provided.
    Missing,
}

/// One source description after resolution.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub uri: String,
    pub target: Target,
}

/// One Arazzo document of the description.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// Where it was retrieved from.
    pub location: String,
    /// Base URI for its relative references: resolved `$self` or `location`.
    pub base: Url,
    pub doc: Arc<Document>,
    /// Its sources by name.
    pub sources: BTreeMap<String, Resolved>,
}

/// The whole description; document 0 is the entry document.
#[derive(Debug, Clone)]
pub struct Description {
    docs: Vec<Loaded>,
}

impl Description {
    /// Loads the description whose entry document was retrieved from `entry`.
    ///
    /// # Errors
    ///
    /// The entry is missing or is not an Arazzo document, or a reference does not
    /// resolve: `$components.*`, a workflow `dependsOn` (unknown or cyclic), a step
    /// or action naming an unknown workflow or step. A missing API document is not
    /// an error here; [`Description::check`] reports it.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::{Description, Sources};
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let mut sources = Sources::new();
    /// sources.insert("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: api, url: api.json, type: openapi } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: ping } ] } ]
    /// "#)?;
    /// sources.insert("https://x/api.json", r#"{"openapi": "3.1.0", "info": {"title": "a", "version": "1"},
    ///     "paths": {"/ping": {"get": {"operationId": "ping"}}}}"#)?;
    /// let d = Description::load(&sources, "https://x/flow.arazzo.yaml")?;
    /// assert!(d.check().is_empty());
    /// assert_eq!(d.entry().workflows[0].workflow_id, "w");
    /// # Ok(())
    /// # }
    /// ```
    pub fn load(sources: &Sources, entry: &str) -> Result<Description> {
        let mut docs: Vec<Loaded> = Vec::new();
        let mut index: BTreeMap<String, usize> = BTreeMap::new();
        let mut queue = vec![absolute(entry)?.to_string()];
        while let Some(uri) = queue.pop() {
            if index.contains_key(&uri) {
                continue;
            }
            let loaded = load_document(sources, &uri, &mut queue)?;
            index.insert(loaded.location.clone(), docs.len());
            if loaded.location != uri {
                index.insert(uri, docs.len());
            }
            docs.push(loaded);
        }
        link_arazzo_sources(sources, &mut docs, &index)?;
        let d = Description { docs };
        for (i, loaded) in d.docs.iter().enumerate() {
            d.verify_references(i).map_err(|e| Error(format!("{}: {e}", loaded.location)))?;
        }
        Ok(d)
    }

    /// Loads a description made of one document retrieved from `uri`.
    ///
    /// Sources that are other documents stay `Target::Missing`.
    ///
    /// # Errors
    ///
    /// As for [`Description::load`].
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Description;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flows/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: api, url: ../specs/api.yaml, type: openapi } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op } ] } ]
    /// "#)?;
    /// assert_eq!(d.source_uri("api").as_deref(), Some("https://x/specs/api.yaml"));
    /// assert!(d.check().iter().any(|p| p.to_string().contains("api.yaml")), "document not loaded");
    /// # Ok(())
    /// # }
    /// ```
    pub fn parse_at(uri: &str, text: &str) -> Result<Description> {
        let mut s = Sources::new();
        s.insert(uri, text)?;
        Description::load(&s, uri)
    }

    /// Every Arazzo document of the description, the entry first.
    ///
    /// `Target::Arazzo(i)` indexes into this list.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Description;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.pu.op } ] } ]
    /// "#)?;
    /// assert_eq!(d.documents().len(), 1);
    /// assert_eq!(d.documents()[0].location, "https://x/flow.arazzo.yaml");
    /// # Ok(())
    /// # }
    /// ```
    pub fn documents(&self) -> &[Loaded] {
        &self.docs
    }

    /// The entry document.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Description;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: demo, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.pu.op } ] } ]
    /// "#)?;
    /// assert_eq!(d.entry().info.title, "demo");
    /// # Ok(())
    /// # }
    /// ```
    pub fn entry(&self) -> &Document {
        &self.docs[0].doc
    }

    /// The entry document's identity: resolved `$self`, if it has one.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Description;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let text = r#"
    /// arazzo: 1.1.0
    /// $self: https://example.com/flow.arazzo.yaml
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.pu.op } ] } ]
    /// "#;
    /// let d = Description::parse_at("file:///cache/abc.yaml", text)?;
    /// assert_eq!(d.self_uri().as_deref(), Some("https://example.com/flow.arazzo.yaml"));
    /// let d = Description::parse_at("file:///cache/abc.yaml", &text.replace("$self: https://example.com/flow.arazzo.yaml\n", ""))?;
    /// assert_eq!(d.self_uri(), None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn self_uri(&self) -> Option<String> {
        self.docs[0].doc.self_uri.as_ref().map(|_| self.docs[0].base.to_string())
    }

    /// The resolved URL of an entry-document source.
    ///
    /// `None` for an unknown name.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Description;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flows/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: ../pu.json } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.pu.op } ] } ]
    /// "#)?;
    /// assert_eq!(d.source_uri("pu").as_deref(), Some("https://x/pu.json"));
    /// assert_eq!(d.source_uri("nope"), None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn source_uri(&self, name: &str) -> Option<String> {
        self.docs[0].sources.get(name).map(|r| r.uri.clone())
    }

    /// A source of document `doc` by name.
    ///
    /// `None` when the document declares no such source.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::description::Target;
    /// use arazzo_jrpc::Description;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: $sourceDescriptions.pu.op } ] } ]
    /// "#)?;
    /// let pu = d.source(0, "pu").expect("declared");
    /// assert!(matches!(pu.target, Target::Opaque));
    /// assert!(d.source(0, "nope").is_none());
    /// # Ok(())
    /// # }
    /// ```
    pub fn source(&self, doc: usize, name: &str) -> Option<&Resolved> {
        self.docs[doc].sources.get(name)
    }

    /// A workflow as a step or action names it from document `from`:
    /// a local `workflowId` or `$sourceDescriptions.<name>.<workflowId>`.
    ///
    /// `None` when the reference does not resolve: unknown id, source not Arazzo,
    /// or source document missing.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Description;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: pu, url: pu.json } ]
    /// workflows: [ { workflowId: mark, steps: [ { stepId: s, operationId: $sourceDescriptions.pu.op } ] } ]
    /// "#)?;
    /// let (doc, w) = d.workflow(0, "mark").expect("local workflow");
    /// assert_eq!((doc, w.workflow_id.as_str()), (0, "mark"));
    /// assert!(d.workflow(0, "$sourceDescriptions.pu.mark").is_none(), "pu is not an Arazzo source");
    /// # Ok(())
    /// # }
    /// ```
    pub fn workflow(&self, from: usize, reference: &str) -> Option<(usize, &Workflow)> {
        match reference.strip_prefix(SOURCE_PREFIX) {
            Some(rest) => {
                let (source, id) = rest.split_once('.')?;
                let Target::Arazzo(k) = self.docs[from].sources.get(source)?.target else { return None };
                self.docs[k].doc.workflow(id).map(|w| (k, w))
            }
            None => self.docs[from].doc.workflow(reference).map(|w| (from, w)),
        }
    }

    /// Everything that can be verified without running: document lints,
    /// missing documents, operations and channels that do not exist, action
    /// directions that contradict the AsyncAPI operation, required OpenAPI
    /// parameters not passed, forward `$steps` references.
    ///
    /// Empty when the description is clean. Paths in documents other than the entry
    /// are prefixed with `<location>#`.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Description;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let d = Description::parse_at("https://x/flow.arazzo.yaml", r#"
    /// arazzo: 1.1.0
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op } ] } ]
    /// "#)?;
    /// let problems = d.check();
    /// assert_eq!(problems.len(), 1);
    /// assert_eq!(problems[0].path, "/sourceDescriptions/0");
    /// # Ok(())
    /// # }
    /// ```
    pub fn check(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        for (i, loaded) in self.docs.iter().enumerate() {
            let prefix = if i == 0 { String::new() } else { format!("{}#", loaded.location) };
            out.extend(self.document_problems(i).into_iter().map(|p| Problem::new(format!("{prefix}{}", p.path), p.message)));
        }
        out
    }
}

/// The operation id without a `$sourceDescriptions.<name>.` prefix.
///
/// The id itself when there is no `$sourceDescriptions.<name>.` prefix.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::description::operation_name;
///
/// assert_eq!(operation_name("$sourceDescriptions.pu.target.mark"), "target.mark");
/// assert_eq!(operation_name("findPets"), "findPets");
/// ```
pub fn operation_name(id: &str) -> &str {
    match id.strip_prefix(SOURCE_PREFIX) {
        Some(rest) => rest.split_once('.').map(|(_, op)| op).unwrap_or(rest),
        None => id,
    }
}

// Private: verification for `load`, findings for `check`.
impl Description {
    /// Checks that every reference of document `i` resolves and workflow
    /// dependencies do not form a cycle.
    fn verify_references(&self, i: usize) -> std::result::Result<(), String> {
        let doc = &self.docs[i].doc;
        for w in &doc.workflows {
            for d in &w.depends_on {
                self.verify_workflow_ref(i, d).map_err(|e| format!("workflow {} dependsOn: {e}", w.workflow_id))?;
            }
            refs::step_parameters(doc, &w.parameters, &[]).map_err(|e| e.0)?;
            for s in &w.steps {
                self.verify_step(i, w, s).map_err(|e| format!("{}/{}: {e}", w.workflow_id, s.step_id))?;
            }
        }
        self.verify_no_cycles(i)
    }

    /// Checks one step: its parameters, target workflow, actions and `dependsOn`.
    fn verify_step(&self, i: usize, w: &Workflow, s: &Step) -> std::result::Result<(), String> {
        let doc = &self.docs[i].doc;
        refs::step_parameters(doc, &w.parameters, &s.parameters).map_err(|e| e.0)?;
        if let StepTarget::WorkflowId(r) = &s.target {
            self.verify_workflow_ref(i, r)?;
        }
        for (own, inherited, success) in [(&s.on_success, &w.success_actions, true), (&s.on_failure, &w.failure_actions, false)] {
            for a in refs::step_actions(doc, inherited, own, success).map_err(|e| e.0)? {
                if let Some(t) = &a.step_id {
                    if w.step(t).is_none() {
                        return Err(format!("action {}: no step {t}", a.name));
                    }
                }
                if let Some(r) = &a.workflow_id {
                    self.verify_workflow_ref(i, r).map_err(|e| format!("action {}: {e}", a.name))?;
                }
                for p in &a.parameters {
                    refs::parameter(doc, p).map_err(|e| e.0)?;
                }
            }
        }
        for dep in &s.depends_on {
            self.verify_step_dependency(i, w, dep).map_err(|e| format!("dependsOn {dep}: {e}"))?;
        }
        Ok(())
    }

    /// Checks that a workflow reference resolves, unless it points into an
    /// Arazzo source whose document was not provided.
    fn verify_workflow_ref(&self, from: usize, reference: &str) -> std::result::Result<(), String> {
        if self.workflow(from, reference).is_none() && !self.unresolvable_source(from, reference) {
            return Err(format!("workflow {reference} not found"));
        }
        Ok(())
    }

    /// Checks one step `dependsOn` entry: a local stepId or
    /// `$workflows.<workflowId>.steps.<stepId>`.
    fn verify_step_dependency(&self, i: usize, w: &Workflow, dep: &str) -> std::result::Result<(), String> {
        if !dep.starts_with('$') {
            return w.step(dep).map(|_| ()).ok_or_else(|| format!("no step {dep} in workflow {}", w.workflow_id));
        }
        if !schema::is_step_reference(dep) {
            return Err("malformed reference".into());
        }
        let (wf_ref, step) = dep.rsplit_once(".steps.").expect("checked by is_step_reference");
        let wf_ref = wf_ref.strip_prefix("$workflows.").unwrap_or(wf_ref);
        match self.workflow(i, wf_ref) {
            Some((_, other)) if other.step(step).is_some() => Ok(()),
            Some(_) => Err(format!("no step {step}")),
            None if self.unresolvable_source(i, wf_ref) => Ok(()),
            None => Err(format!("no workflow {wf_ref}")),
        }
    }

    /// Checks that workflow `dependsOn` of document `i` is acyclic (within and across documents).
    fn verify_no_cycles(&self, i: usize) -> std::result::Result<(), String> {
        let mut done = BTreeSet::new();
        for w in &self.docs[i].doc.workflows {
            self.visit_dependencies((i, w.workflow_id.clone()), &mut Vec::new(), &mut done)?;
        }
        Ok(())
    }

    /// Depth-first walk over workflow `dependsOn` from `node` = (document, workflowId).
    fn visit_dependencies(&self, node: (usize, String), path: &mut Vec<(usize, String)>, done: &mut BTreeSet<(usize, String)>) -> std::result::Result<(), String> {
        if done.contains(&node) {
            return Ok(());
        }
        if path.contains(&node) {
            let names: Vec<&str> = path.iter().map(|(_, w)| w.as_str()).collect();
            return Err(format!("cycle in dependsOn: {} -> {}", names.join(" → "), node.1));
        }
        let Some(w) = self.docs[node.0].doc.workflow(&node.1) else { return Ok(()) };
        path.push(node.clone());
        for dep in &w.depends_on {
            if let Some((k, next)) = self.workflow(node.0, dep) {
                self.visit_dependencies((k, next.workflow_id.clone()), path, done)?;
            }
        }
        path.pop();
        done.insert(node);
        Ok(())
    }

    /// Whether `reference` points into an Arazzo source whose document was
    /// not provided — such references are reported by `check`, not `load`.
    fn unresolvable_source(&self, from: usize, reference: &str) -> bool {
        reference
            .strip_prefix(SOURCE_PREFIX)
            .and_then(|r| r.split('.').next())
            .and_then(|s| self.docs[from].sources.get(s))
            .is_some_and(|r| matches!(r.target, Target::Missing | Target::Opaque))
    }

    /// Every finding in document `i`, located inside that document.
    fn document_problems(&self, i: usize) -> Vec<Problem> {
        let loaded = &self.docs[i];
        let mut out = loaded.doc.lint();
        for (k, s) in loaded.doc.source_descriptions.iter().enumerate() {
            if let Some(Resolved { uri, target: Target::Missing }) = loaded.sources.get(&s.name) {
                out.push(Problem::new(format!("/sourceDescriptions/{k}"), format!("source {}: document {uri} not loaded", s.name)));
            }
        }
        for (wi, w) in loaded.doc.workflows.iter().enumerate() {
            for (si, s) in w.steps.iter().enumerate() {
                let at = format!("/workflows/{wi}/steps/{si}");
                out.extend(self.check_step(i, w, s).into_iter().map(|m| Problem::new(at.clone(), format!("step {}: {m}", s.step_id))));
            }
            out.extend(order_problems(w).into_iter().map(|m| Problem::new(format!("/workflows/{wi}"), m)));
        }
        out
    }

    /// What is wrong with one step against its source's API description.
    ///
    /// Empty when the step checks out or its source has no document to check against.
    fn check_step(&self, i: usize, w: &Workflow, s: &Step) -> Vec<String> {
        let doc = &self.docs[i].doc;
        let Some(name) = doc.step_source(s) else {
            return match &s.target {
                StepTarget::OperationId(id) if !id.starts_with('$') => {
                    vec![format!("operationId {id}: several sources declare it, write $sourceDescriptions.<name>.{id}")]
                }
                _ => vec![],
            };
        };
        let Some(resolved) = self.source(i, name) else {
            return vec![format!("source {name} is not declared in sourceDescriptions")];
        };
        match (&s.target, &resolved.target) {
            (StepTarget::OperationId(id), Target::OpenApi(api)) => {
                let op_id = operation_name(id);
                match openapi::find_operation(api, op_id) {
                    None => vec![format!("operation {op_id} not found in {name}")],
                    Some(op) => missing_parameters(doc, w, s, &op),
                }
            }
            (StepTarget::OperationPath(p), Target::OpenApi(api)) => {
                let (_, pointer) = split_source_path(p).expect("source was resolved from the path");
                match openapi::at_pointer(api, pointer) {
                    None => vec![format!("operationPath {p}: no operation at {pointer}")],
                    Some(op) => missing_parameters(doc, w, s, &op),
                }
            }
            (StepTarget::OperationId(id), Target::AsyncApi(api)) => {
                let op_id = operation_name(id);
                match asyncapi::find_operation(api, op_id) {
                    None => vec![format!("operation {op_id} not found in {name}")],
                    Some(op) => match s.action {
                        Some(a) if a != op.action => {
                            vec![format!("action {} contradicts operation {op_id} ({})", action_name(a), action_name(op.action))]
                        }
                        _ => vec![],
                    },
                }
            }
            (StepTarget::ChannelPath(p), Target::AsyncApi(api)) => {
                let reference = split_source_path(p).map(|(_, ptr)| ptr).unwrap_or(p);
                match asyncapi::find_channel(api, reference) {
                    None => vec![format!("channelPath {p}: channel not found in {name}")],
                    Some(_) => vec![],
                }
            }
            _ => vec![],
        }
    }
}

/// Loads the Arazzo document at `uri` and resolves its sources.
///
/// Arazzo sources still hold a placeholder index; their locations are pushed to `queue`.
fn load_document(sources: &Sources, uri: &str, queue: &mut Vec<String>) -> Result<Loaded> {
    let Some((location, SourceDoc::Arazzo(doc))) = sources.lookup(uri) else {
        return Err(Error(format!("{uri}: Arazzo document not among the loaded ones")));
    };
    let location = absolute(location)?;
    let base = match &doc.self_uri {
        Some(s) => location.join(s).map_err(|e| Error(format!("$self {s:?}: {e}")))?,
        None => location.clone(),
    };
    let resolved = resolve_sources(sources, doc, &base, queue)?;
    Ok(Loaded { location: location.to_string(), base, doc: doc.clone(), sources: resolved })
}

/// Resolves each source URL of `doc` against `base` and finds what it points at.
///
/// An Arazzo source gets `Target::Arazzo(usize::MAX)` and its location is pushed to `queue`.
fn resolve_sources(sources: &Sources, doc: &Document, base: &Url, queue: &mut Vec<String>) -> Result<BTreeMap<String, Resolved>> {
    let mut resolved = BTreeMap::new();
    for s in &doc.source_descriptions {
        let uri = base.join(&s.url).map_err(|e| Error(format!("source {}: url {:?}: {e}", s.name, s.url)))?.to_string();
        let target = match sources.lookup(&uri) {
            Some((loc, SourceDoc::Arazzo(_))) => {
                queue.push(loc.to_string());
                Target::Arazzo(usize::MAX) // index is filled in by link_arazzo_sources
            }
            Some((_, SourceDoc::OpenApi(v))) => Target::OpenApi(v.clone()),
            Some((_, SourceDoc::AsyncApi(v))) => Target::AsyncApi(v.clone()),
            None if s.kind.is_none() => Target::Opaque,
            None => Target::Missing,
        };
        resolved.insert(s.name.clone(), Resolved { uri, target });
    }
    Ok(resolved)
}

/// Replaces the placeholder of every Arazzo source with the index of its loaded document.
///
/// `index` maps retrieval and requested URIs to positions in `docs`.
fn link_arazzo_sources(sources: &Sources, docs: &mut [Loaded], index: &BTreeMap<String, usize>) -> Result<()> {
    for loaded in docs {
        for r in loaded.sources.values_mut() {
            if let Target::Arazzo(slot) = &mut r.target {
                let (loc, _) = sources.lookup(&r.uri).expect("found during the walk");
                *slot = index[&absolute(loc)?.to_string()];
            }
        }
    }
    Ok(())
}

/// The spec spelling of an AsyncAPI action.
fn action_name(a: AsyncAction) -> &'static str {
    match a {
        AsyncAction::Send => "send",
        AsyncAction::Receive => "receive",
    }
}

/// Required OpenAPI parameters (and every path parameter) the step does not
/// pass, counting the workflow's parameters.
///
/// Empty when the step's parameters do not resolve; `load` reports that.
fn missing_parameters(doc: &Document, w: &Workflow, s: &Step, op: &openapi::Operation) -> Vec<String> {
    let Ok(given) = refs::step_parameters(doc, &w.parameters, &s.parameters) else { return vec![] };
    let passed = |p: &Value| {
        given.iter().any(|g| {
            let loc = g.location.map(|l| serde_json::to_value(l).unwrap());
            p["name"] == g.name.as_str() && loc.is_none_or(|l| l == p["in"])
        })
    };
    op.parameters()
        .into_iter()
        .filter(|p| p["required"] == true || p["in"] == "path")
        .filter(|p| !passed(p))
        .map(|p| format!("operation requires parameter {} ({})", p["name"].as_str().unwrap_or("?"), p["in"].as_str().unwrap_or("?")))
        .collect()
}

/// Step dependencies a sequential run cannot satisfy (spec 5.8.5.2.4–5):
/// every explicit `dependsOn` (a local `stepId`) must come earlier in the
/// array, every implicit one — a `$steps.<id>` reference — earlier or be the
/// step itself (its actions see its outputs). A workflow that jumps (`goto` /
/// `retry` to a step, or an action by reference) does not run in array
/// order, so it is left to the run-time check (`run::check_dependencies`).
fn order_problems(w: &Workflow) -> Vec<String> {
    const STEPS: &str = "$steps.";
    let jumps = |list: &[ActionOrRef]| list.iter().any(|a| a.as_action().is_none_or(|a| a.step_id.is_some()));
    if jumps(&w.success_actions) || jumps(&w.failure_actions) || w.steps.iter().any(|s| jumps(&s.on_success) || jumps(&s.on_failure)) {
        return vec![];
    }
    let mut out = Vec::new();
    for (i, s) in w.steps.iter().enumerate() {
        for id in s.depends_on.iter().filter(|d| !d.starts_with('$')) {
            if w.step_index(id).is_some_and(|j| j >= i) {
                out.push(format!("step {}: dependsOn {id}: step {id} is not placed before {}", s.step_id, s.step_id));
            }
        }
        let text = serde_json::to_string(s).unwrap_or_default();
        for (pos, _) in text.match_indices(STEPS) {
            let id: String = text[pos + STEPS.len()..].chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect();
            if w.step_index(&id).is_some_and(|j| j > i) {
                out.push(format!("step {}: forward reference $steps.{id}: step {id} runs later", s.step_id));
            }
        }
    }
    out
}
