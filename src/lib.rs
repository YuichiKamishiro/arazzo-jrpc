#![doc = include_str!("../README.md")]
//! ## Module map
//!
//! Load: [`sources`] (texts by URI, `$self` identity) → [`document`] (one
//! Arazzo document: parse, official schema + `schema` walker, lint) →
//! [`description`] (the documents linked through `sourceDescriptions`,
//! references verified, `check()` against the API descriptions: [`openapi`],
//! [`asyncapi`]); [`refs`] resolves `$components` and inherited
//! parameters/actions.
//!
//! Run: [`exec`] (`Engine` → `Session` → workflow walk, steps, actions) →
//! [`inputs`] (JSON Schema defaults and validation), [`payload`] (parameter
//! values, request bodies, replacements), [`expr`] (runtime expressions),
//! [`condition`] (criteria), [`select`] (Selector Objects, XPath via `xpath`
//! with the `xpath` feature) → [`transport`] (HTTP, messages, opaque `Invoker`).
//!
//! [`model`] holds the spec types; [`error`] the crate `Error` and `Problem`.

pub mod asyncapi;
pub mod condition;
pub mod description;
pub mod document;
pub mod error;
pub mod exec;
pub mod expr;
pub mod inputs;
pub mod model;
pub mod openapi;
pub mod payload;
pub mod refs;
mod schema;
pub mod select;
pub mod sources;
pub mod transport;
#[cfg(feature = "xpath")]
mod xpath;

pub use error::{Error, Problem, Result};
pub use exec::{Done, Engine, Outcome, Session};
pub use description::Description;
pub use model::Document;
pub use sources::Sources;
