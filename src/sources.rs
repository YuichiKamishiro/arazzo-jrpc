//! The set of documents a description may be made of, keyed by the URI they
//! were retrieved from. The crate does no I/O: the caller reads files or
//! fetches URLs and hands the texts over.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use url::Url;

use crate::document::parse_value;
use crate::error::{Error, Result};
use crate::model::Document;

/// What kind of document a retrieved text turned out to be.
#[derive(Debug, Clone)]
pub enum SourceDoc {
    Arazzo(Arc<Document>),
    OpenApi(Arc<Value>),
    AsyncApi(Arc<Value>),
}

/// Documents by retrieval URI, plus an identity index of Arazzo `$self`.
#[derive(Debug, Default, Clone)]
pub struct Sources {
    by_location: BTreeMap<String, SourceDoc>,
    by_identity: BTreeMap<String, String>,
}

impl Sources {
    /// Creates an empty set.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Sources;
    ///
    /// let sources = Sources::new();
    /// assert!(sources.lookup("https://x/api.yaml").is_none());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses one document retrieved from `uri` and remembers it.
    ///
    /// # Errors
    ///
    /// The text is not YAML/JSON, not an object, not a known kind (`arazzo`,
    /// `openapi` or `asyncapi` at the root), or an invalid Arazzo document.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::sources::SourceDoc;
    /// use arazzo_jrpc::Sources;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let mut sources = Sources::new();
    /// sources.insert("https://x/api.yaml", "openapi: 3.1.0\ninfo: { title: a, version: '1' }\npaths: {}\n")?;
    /// assert!(matches!(sources.lookup("https://x/api.yaml"), Some((_, SourceDoc::OpenApi(_)))));
    /// assert!(sources.insert("https://x/other.yaml", "just: yaml\n").is_err(), "not a known document kind");
    /// assert!(sources.insert("relative.yaml", "openapi: 3.1.0\n").is_err(), "the URI must be absolute");
    /// # Ok(())
    /// # }
    /// ```
    pub fn insert(&mut self, uri: &str, text: &str) -> Result<()> {
        let location = absolute(uri)?;
        let value = parse_value(text)?;
        let Some(root) = value.as_object() else {
            return Err(Error(format!("{uri}: document must be an object")));
        };
        let doc = if root.contains_key("arazzo") {
            self.arazzo(&location, uri, value)?
        } else if root.contains_key("openapi") || root.contains_key("swagger") {
            SourceDoc::OpenApi(Arc::new(value))
        } else if root.contains_key("asyncapi") {
            SourceDoc::AsyncApi(Arc::new(value))
        } else {
            return Err(Error(format!("{uri}: unknown document kind: none of arazzo, openapi, asyncapi")));
        };
        self.by_location.insert(location.to_string(), doc);
        Ok(())
    }

    /// Finds a document by absolute URI: identity (`$self`) first, then
    /// retrieval location (spec 5.5.2).
    ///
    /// Gives the retrieval URI and the document.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::Sources;
    ///
    /// # fn main() -> arazzo_jrpc::Result<()> {
    /// let mut sources = Sources::new();
    /// sources.insert("file:///cache/abc.yaml", r#"
    /// arazzo: 1.1.0
    /// $self: https://canonical.example.com/shared.arazzo.yaml
    /// info: { title: t, version: '1' }
    /// sourceDescriptions: [ { name: api, url: api.yaml, type: openapi } ]
    /// workflows: [ { workflowId: w, steps: [ { stepId: s, operationId: op } ] } ]
    /// "#)?;
    /// let (location, _) = sources.lookup("https://canonical.example.com/shared.arazzo.yaml").expect("found by $self");
    /// assert_eq!(location, "file:///cache/abc.yaml");
    /// assert!(sources.lookup("file:///cache/abc.yaml").is_some(), "and by location");
    /// # Ok(())
    /// # }
    /// ```
    pub fn lookup(&self, uri: &str) -> Option<(&str, &SourceDoc)> {
        let location = self.by_identity.get(uri).map(String::as_str).unwrap_or(uri);
        self.by_location.get_key_value(location).map(|(k, v)| (k.as_str(), v))
    }

    /// Validates an Arazzo text and indexes its `$self` (resolved against
    /// the retrieval location) for [`Sources::lookup`].
    fn arazzo(&mut self, location: &Url, uri: &str, value: Value) -> Result<SourceDoc> {
        let doc = Document::from_value(value).map_err(|e| Error(format!("{uri}: {e}")))?;
        if let Some(own) = &doc.self_uri {
            let identity = location.join(own).map_err(|e| Error(format!("{uri}: $self {own:?}: {e}")))?;
            self.by_identity.insert(identity.to_string(), location.to_string());
        }
        Ok(SourceDoc::Arazzo(Arc::new(doc)))
    }
}

/// A retrieval URI as an absolute URL.
pub(crate) fn absolute(uri: &str) -> Result<Url> {
    Url::parse(uri).map_err(|e| Error(format!("{uri}: absolute URI required ({e})")))
}
