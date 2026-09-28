//! How steps reach the outside world. The executor builds a request, a
//! transport performs it; nothing here knows about workflows.
//!
//! Three kinds, by what a source is:
//! - OpenAPI → [`HttpTransport`] (built-in [`http::ReqwestHttp`] with the
//!   `http` feature, or the caller's);
//! - AsyncAPI → [`MessageTransport`] ([`InMemoryBroker`] for tests, or a
//!   caller's Kafka/AMQP/MQTT adapter);
//! - untyped (opaque) sources → the caller's [`Invoker`], e.g. JSON-RPC.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use serde_json::Value;

use crate::model::SourceDescription;

#[cfg(feature = "http")]
pub mod http;

/// A transport-level failure: no response at all (refused, timed out…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError(pub String);

impl TransportError {
    /// Wraps a message.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::TransportError;
    ///
    /// let e = TransportError::new("connection refused");
    /// assert_eq!(e.to_string(), "connection refused");
    /// ```
    pub fn new(message: impl Into<String>) -> Self {
        TransportError(message.into())
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An HTTP request as the executor built it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// The step `timeout`, if it has one.
    pub timeout: Option<Duration>,
}

/// An HTTP response as a transport received it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Performs HTTP requests for OpenAPI steps.
///
/// # Examples
///
/// A transport that answers every request with `200 {}`:
///
/// ```
/// use arazzo_jrpc::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};
///
/// struct AlwaysOk;
///
/// impl HttpTransport for AlwaysOk {
///     fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
///         Ok(HttpResponse {
///             status: 200,
///             headers: vec![("Content-Type".into(), "application/json".into())],
///             body: b"{}".to_vec(),
///         })
///     }
/// }
///
/// let response = AlwaysOk.send(&HttpRequest { method: "GET".into(), url: "https://x/".into(), ..Default::default() }).unwrap();
/// assert_eq!(response.status, 200);
/// ```
pub trait HttpTransport {
    /// Sends one request.
    ///
    /// A 500 is a response, not an error.
    ///
    /// # Errors
    ///
    /// No response at all: refused, timed out.
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// One completed HTTP exchange as runtime expressions see it: `$url`,
/// `$method`, `$statusCode`, `$request.*`, `$response.*`.
#[derive(Debug, Clone, Default)]
pub struct Exchange {
    pub url: String,
    pub method: String,
    pub request_headers: Vec<(String, String)>,
    pub request_query: Vec<(String, String)>,
    pub request_path: Vec<(String, String)>,
    pub request_body: Option<Value>,
    pub status: u16,
    pub response_headers: Vec<(String, String)>,
    pub response_body: Value,
}

/// An AsyncAPI message, sent or received.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// The resolved channel address.
    pub channel: String,
    pub headers: Vec<(String, String)>,
    pub payload: Value,
}

/// The header that carries a message's correlation id; a receive step's
/// `correlationId` is matched against it.
pub const CORRELATION_HEADER: &str = "correlationId";

impl Message {
    /// A message without headers.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::Message;
    /// use serde_json::json;
    ///
    /// let m = Message::new("orders", json!({"id": 1}));
    /// assert_eq!(m.channel, "orders");
    /// assert!(m.headers.is_empty());
    /// ```
    pub fn new(channel: &str, payload: Value) -> Self {
        Message { channel: channel.into(), headers: vec![], payload }
    }

    /// A message carrying `correlation` in the [`CORRELATION_HEADER`] header.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::{Message, CORRELATION_HEADER};
    /// use serde_json::json;
    ///
    /// let m = Message::correlated("orders", "c-1", json!({}));
    /// assert_eq!(m.header(CORRELATION_HEADER), Some("c-1"));
    /// ```
    pub fn correlated(channel: &str, correlation: &str, payload: Value) -> Self {
        Message {
            channel: channel.into(),
            headers: vec![(CORRELATION_HEADER.into(), correlation.into())],
            payload,
        }
    }

    /// Header value by case-insensitive name.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::Message;
    /// use serde_json::json;
    ///
    /// let m = Message::correlated("orders", "c-1", json!({}));
    /// assert_eq!(m.header("correlationid"), Some("c-1"));
    /// assert_eq!(m.header("nope"), None);
    /// ```
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

/// Header lookup by case-insensitive name (RFC 9110 field names).
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::transport::header;
///
/// let headers = [("Content-Type".to_string(), "application/json".to_string())];
/// assert_eq!(header(&headers, "content-type"), Some("application/json"));
/// assert_eq!(header(&headers, "Accept"), None);
/// ```
pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// Sends and receives AsyncAPI messages.
///
/// # Examples
///
/// [`InMemoryBroker`] implements it; the same calls the engine makes:
///
/// ```
/// use std::time::Duration;
///
/// use arazzo_jrpc::transport::{InMemoryBroker, Message, MessageTransport};
/// use serde_json::json;
///
/// # fn main() -> Result<(), arazzo_jrpc::transport::TransportError> {
/// let mut bus = InMemoryBroker::default();
/// bus.subscribe("orders.confirmed")?;
/// bus.send(&Message::new("orders.eu", json!({"productId": "p"})))?;
/// bus.queue(Message::correlated("orders.confirmed", "c-1", json!({"status": "ok"})));
/// let reply = bus.receive("orders.confirmed", Some("c-1"), Duration::from_millis(50))?;
/// assert_eq!(reply.map(|m| m.payload), Some(json!({"status": "ok"})));
/// assert_eq!(bus.receive("orders.confirmed", None, Duration::from_millis(50))?, None);
/// # Ok(())
/// # }
/// ```
pub trait MessageTransport {
    /// Publishes one message; a send step completes right after (5.8.5.3).
    ///
    /// # Errors
    ///
    /// The broker refused the message.
    fn send(&mut self, message: &Message) -> Result<(), TransportError>;

    /// Waits up to `timeout` for a message on `channel` whose correlation id
    /// equals `correlation` (any message when `None`).
    ///
    /// `None` when the timeout passed.
    ///
    /// # Errors
    ///
    /// A transport-level failure; a timeout is `Ok(None)`, not an error.
    fn receive(&mut self, channel: &str, correlation: Option<&str>, timeout: Duration) -> Result<Option<Message>, TransportError>;

    /// Starts keeping every message whose address fits `channel` — an
    /// AsyncAPI channel address, possibly a template like `orders.{region}` —
    /// until a receive takes it. The engine calls it before a run's first
    /// step for every receive channel of the description, so a reply
    /// published while earlier steps run is not lost.
    ///
    /// # Errors
    ///
    /// The broker refused the subscription.
    fn subscribe(&mut self, channel: &str) -> Result<(), TransportError>;
}

/// A broker in memory for tests: queued messages are "received", sent ones
/// are recorded.
///
/// A reply registered with [`InMemoryBroker::reply_on`] is published live
/// when its trigger is sent and, like on a real broker, kept only for a
/// subscribed channel. Waiting never sleeps — the requested timeout is
/// recorded instead.
#[derive(Debug, Default)]
pub struct InMemoryBroker {
    inbox: VecDeque<Message>,
    outbox: Vec<Message>,
    waits: Vec<Duration>,
    subscriptions: Vec<String>,
    /// One-shot replies: (channel whose send triggers it, the reply).
    replies: Vec<(String, Message)>,
}

impl InMemoryBroker {
    /// Makes `message` available to a later receive — already delivered,
    /// subscription or not (a retained message).
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use arazzo_jrpc::transport::{InMemoryBroker, Message, MessageTransport};
    /// use serde_json::json;
    ///
    /// let mut bus = InMemoryBroker::default();
    /// bus.queue(Message::new("orders.confirmed", json!({"id": 1})));
    /// let got = bus.receive("orders.confirmed", None, Duration::from_millis(1)).unwrap();
    /// assert_eq!(got.map(|m| m.payload), Some(json!({"id": 1})));
    /// ```
    pub fn queue(&mut self, message: Message) {
        self.inbox.push_back(message);
    }

    /// Every message sent so far, in order.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::{InMemoryBroker, Message, MessageTransport};
    /// use serde_json::json;
    ///
    /// let mut bus = InMemoryBroker::default();
    /// bus.send(&Message::new("orders.eu", json!({}))).unwrap();
    /// assert_eq!(bus.published()[0].channel, "orders.eu");
    /// ```
    pub fn published(&self) -> &[Message] {
        &self.outbox
    }

    /// Publishes `reply` live the next time something is sent on `sent`; it
    /// is kept only if a subscription covers its channel, otherwise lost.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use arazzo_jrpc::transport::{InMemoryBroker, Message, MessageTransport};
    /// use serde_json::json;
    ///
    /// let mut bus = InMemoryBroker::default();
    /// bus.reply_on("orders.eu", Message::new("orders.confirmed", json!({"n": 1})));
    /// bus.send(&Message::new("orders.eu", json!({}))).unwrap();
    /// assert_eq!(bus.receive("orders.confirmed", None, Duration::from_millis(1)).unwrap(), None, "no subscription: lost");
    ///
    /// bus.subscribe("orders.confirmed").unwrap();
    /// bus.reply_on("orders.eu", Message::new("orders.confirmed", json!({"n": 2})));
    /// bus.send(&Message::new("orders.eu", json!({}))).unwrap();
    /// assert!(bus.receive("orders.confirmed", None, Duration::from_millis(1)).unwrap().is_some());
    /// ```
    pub fn reply_on(&mut self, sent: &str, reply: Message) {
        self.replies.push((sent.to_string(), reply));
    }

    /// Channels subscribed so far, in order.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::{InMemoryBroker, MessageTransport};
    ///
    /// let mut bus = InMemoryBroker::default();
    /// bus.subscribe("orders.confirmed").unwrap();
    /// bus.subscribe("orders.confirmed").unwrap();
    /// assert_eq!(bus.subscribed(), ["orders.confirmed"]);
    /// ```
    pub fn subscribed(&self) -> &[String] {
        &self.subscriptions
    }

    /// The timeouts of receives that found nothing.
    ///
    /// One entry per receive step that timed out.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use arazzo_jrpc::transport::{InMemoryBroker, MessageTransport};
    ///
    /// let mut bus = InMemoryBroker::default();
    /// bus.receive("orders.confirmed", None, Duration::from_millis(250)).unwrap();
    /// assert_eq!(bus.waited(), [Duration::from_millis(250)]);
    /// ```
    pub fn waited(&self) -> &[Duration] {
        &self.waits
    }
}

impl MessageTransport for InMemoryBroker {
    fn send(&mut self, message: &Message) -> Result<(), TransportError> {
        self.outbox.push(message.clone());
        let (fired, rest) = std::mem::take(&mut self.replies).into_iter().partition(|(sent, _)| *sent == message.channel);
        self.replies = rest;
        for (_, reply) in fired {
            if self.subscriptions.iter().any(|t| covers(t, &reply.channel)) {
                self.inbox.push_back(reply);
            }
        }
        Ok(())
    }

    fn subscribe(&mut self, channel: &str) -> Result<(), TransportError> {
        if !self.subscriptions.iter().any(|c| c == channel) {
            self.subscriptions.push(channel.to_string());
        }
        Ok(())
    }

    /// Takes the first queued message on `channel` with a matching correlation id.
    fn receive(&mut self, channel: &str, correlation: Option<&str>, timeout: Duration) -> Result<Option<Message>, TransportError> {
        let matches = |m: &Message| m.channel == channel && correlation.is_none_or(|c| m.header(CORRELATION_HEADER) == Some(c));
        match self.inbox.iter().position(matches) {
            Some(i) => Ok(self.inbox.remove(i)),
            None => {
                self.waits.push(timeout);
                Ok(None)
            }
        }
    }
}

/// Whether `address` fits the channel `template`; `{name}` stands for a
/// non-empty run without `.` or `/`.
fn covers(template: &str, address: &str) -> bool {
    let Some((lit, rest)) = template.split_once('{') else {
        return template == address;
    };
    let Some(tail) = address.strip_prefix(lit) else {
        return false;
    };
    let after = rest.split_once('}').map_or("", |(_, a)| a);
    tail.char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .take_while(|&e| !tail[..e].contains(['.', '/']))
        .any(|e| covers(after, &tail[e..]))
}

/// A call to an operation of an opaque (untyped) source.
#[derive(Debug)]
pub struct Call<'a> {
    pub source: &'a SourceDescription,
    /// The operation name without the `$sourceDescriptions.<name>.` prefix.
    pub operation: String,
    /// Evaluated parameters: `(name, in, value)`; `in` as written (may be absent).
    pub parameters: Vec<(String, Option<String>, Value)>,
    /// Evaluated `requestBody.payload`.
    pub payload: Option<Value>,
    pub timeout: Option<Duration>,
}

/// A failed opaque call. `body` is what `onFailure` criteria see as
/// `$response.body` (for JSON-RPC, the error object).
#[derive(Debug, Clone, PartialEq)]
pub struct CallFailure {
    pub message: String,
    pub body: Value,
}

impl CallFailure {
    /// A failure with the text shown in reports and the body criteria inspect.
    ///
    /// # Examples
    ///
    /// ```
    /// use arazzo_jrpc::transport::CallFailure;
    /// use serde_json::json;
    ///
    /// let f = CallFailure::new("exec=17", json!({"code": -32000, "data": {"exec": 17}}));
    /// assert_eq!(f.body["data"]["exec"], 17);
    /// ```
    pub fn new(message: impl Into<String>, body: Value) -> Self {
        CallFailure { message: message.into(), body }
    }
}

/// Serves operations of opaque sources (no `type`), e.g. JSON-RPC
/// control ports.
///
/// # Examples
///
/// A JSON-RPC-like invoker that fails `target.mark` with an error object
/// `onFailure` criteria can read as `$response.body#/data/exec`:
///
/// ```
/// use arazzo_jrpc::transport::{Call, CallFailure, Invoker};
/// use serde_json::{json, Value};
///
/// struct Port;
///
/// impl Invoker for Port {
///     fn invoke(&mut self, call: &Call) -> Result<Value, CallFailure> {
///         match call.operation.as_str() {
///             "target.mark" => Err(CallFailure::new("exec=17", json!({"code": -32000, "data": {"exec": 17}}))),
///             _ => Ok(json!({"ok": true})),
///         }
///     }
/// }
/// ```
pub trait Invoker {
    /// Performs one call.
    ///
    /// The body becomes `$response.body`.
    ///
    /// # Errors
    ///
    /// The call failed; the failure's body is what `onFailure` criteria see.
    fn invoke(&mut self, call: &Call) -> Result<Value, CallFailure>;
}
