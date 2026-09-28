//! AsyncAPI descriptions (2.x and 3.0): which channel an operation uses and
//! in which direction.

use serde_json::Value;

use crate::model::AsyncAction;
use crate::openapi::unescape;

/// A channel: its key in `channels` and the address messages travel on.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub key: String,
    pub address: String,
}

/// An operation: which way it goes and on which channel.
#[derive(Debug, Clone, PartialEq)]
pub struct AsyncOperation {
    pub action: AsyncAction,
    pub channel: Channel,
}

/// Finds the operation with this id.
///
/// 3.0: `operations.<id>` with `action` and a `channel` `$ref`.
/// 2.x: `channels.<address>.publish|subscribe.operationId`; the client
/// sends on `publish` and receives on `subscribe`.
/// `None` when the document has no operation with that id.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::asyncapi;
/// use arazzo_jrpc::model::AsyncAction;
/// use serde_json::json;
///
/// let bus = json!({
///     "asyncapi": "3.0.0",
///     "channels": {"orders": {"address": "orders.{region}"}},
///     "operations": {"placeOrder": {"action": "send", "channel": {"$ref": "#/channels/orders"}}}
/// });
/// let op = asyncapi::find_operation(&bus, "placeOrder").expect("declared");
/// assert_eq!(op.action, AsyncAction::Send);
/// assert_eq!(op.channel.address, "orders.{region}");
/// assert!(asyncapi::find_operation(&bus, "nope").is_none());
/// ```
pub fn find_operation(doc: &Value, id: &str) -> Option<AsyncOperation> {
    if is_v2(doc) {
        operation_v2(doc, id)
    } else {
        operation_v3(doc, id)
    }
}

/// Finds a channel by pointer (`/channels/orders`), by key, or by address.
///
/// A 3.0 channel without `address` uses its key. `None` when nothing matches.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::asyncapi;
/// use serde_json::json;
///
/// let bus = json!({"asyncapi": "3.0.0", "channels": {
///     "orders": {"address": "orders.{region}"},
///     "confirmations": {}
/// }});
/// assert_eq!(asyncapi::find_channel(&bus, "/channels/orders").map(|c| c.address), Some("orders.{region}".into()));
/// assert_eq!(asyncapi::find_channel(&bus, "confirmations").map(|c| c.address), Some("confirmations".into()));
/// assert!(asyncapi::find_channel(&bus, "nope").is_none());
/// ```
pub fn find_channel(doc: &Value, reference: &str) -> Option<Channel> {
    let key = match reference.strip_prefix("/channels/") {
        Some(k) => unescape(k),
        None => reference.to_string(),
    };
    let channels = doc["channels"].as_object()?;
    if let Some(ch) = channels.get(&key) {
        let address = ch["address"].as_str().unwrap_or(&key).to_string();
        return Some(Channel { key, address });
    }
    channels
        .iter()
        .find(|(_, ch)| ch["address"] == reference)
        .map(|(k, _)| Channel { key: k.clone(), address: reference.to_string() })
}

/// Whether the document is AsyncAPI 2.x (`asyncapi: 2.6.0`).
fn is_v2(doc: &Value) -> bool {
    doc["asyncapi"].as_str().is_some_and(|v| v.starts_with('2'))
}

/// Finds a 2.x operation: `channels.<address>.publish|subscribe.operationId`.
fn operation_v2(doc: &Value, id: &str) -> Option<AsyncOperation> {
    for (address, ch) in doc["channels"].as_object()? {
        for (verb, action) in [("publish", AsyncAction::Send), ("subscribe", AsyncAction::Receive)] {
            if ch[verb]["operationId"] == id {
                let channel = Channel { key: address.clone(), address: address.clone() };
                return Some(AsyncOperation { action, channel });
            }
        }
    }
    None
}

/// Finds a 3.0 operation: `operations.<id>` with `action` and a `#/channels/<key>` `$ref`.
fn operation_v3(doc: &Value, id: &str) -> Option<AsyncOperation> {
    let op = doc["operations"].get(id)?;
    let action = match op["action"].as_str()? {
        "send" => AsyncAction::Send,
        "receive" => AsyncAction::Receive,
        _ => return None,
    };
    let key = op["channel"]["$ref"].as_str()?.strip_prefix("#/channels/")?;
    Some(AsyncOperation { action, channel: find_channel(doc, &unescape(key))? })
}
