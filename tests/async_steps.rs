//! AsyncAPI steps (spec 5.8.5, 5.8.5.2, 5.8.5.3): send completes on publish,
//! receive waits for a message matching correlationId until timeout,
//! `$message.*`, channelPath, channel parameters, and step-level dependsOn
//! in a sequential executor.

use std::time::Duration;

use arazzo_jrpc::transport::{InMemoryBroker, Message};
use arazzo_jrpc::{Description, Engine, Sources};
use serde_json::json;

const ENTRY: &str = "https://example.com/flows/orders.arazzo.yaml";
const BUS: &str = "https://example.com/specs/orders.asyncapi.yaml";

fn asyncapi() -> String {
    json!({
      "asyncapi": "3.0.0",
      "info": {"title": "orders", "version": "1"},
      "channels": {
        "orders": {"address": "orders.{region}", "parameters": {"region": {}}},
        "confirmations": {"address": "orders.confirmed"}
      },
      "operations": {
        "placeOrder": {"action": "send", "channel": {"$ref": "#/channels/orders"}},
        "confirmOrder": {"action": "receive", "channel": {"$ref": "#/channels/confirmations"}}
      }
    })
    .to_string()
}

fn describe(workflows: &str) -> Description {
    let text = format!(
        "arazzo: 1.1.0\ninfo: {{ title: t, version: '1' }}\n\
         sourceDescriptions: [ {{ name: bus, url: ../specs/orders.asyncapi.yaml, type: asyncapi }} ]\n\
         workflows:\n{workflows}"
    );
    let mut s = Sources::new();
    s.insert(ENTRY, &text).unwrap();
    s.insert(BUS, &asyncapi()).unwrap();
    Description::load(&s, ENTRY).unwrap()
}

const PLACE_AND_CONFIRM: &str = "  - workflowId: w\n    inputs: { type: object }\n\
    \x20   outputs: { orderId: $steps.confirm.outputs.orderId }\n    steps:\n\
    \x20     - stepId: place\n        operationId: $sourceDescriptions.bus.placeOrder\n        action: send\n\
    \x20       parameters:\n          - { name: correlationId, in: header, value: $inputs.cid }\n          - { name: region, in: channel, value: eu }\n\
    \x20       requestBody: { payload: { productId: $inputs.product } }\n\
    \x20     - stepId: confirm\n        operationId: $sourceDescriptions.bus.confirmOrder\n        action: receive\n\
    \x20       correlationId: $inputs.cid\n        timeout: 200\n        dependsOn: [ place ]\n\
    \x20       successCriteria: [ { condition: $message.payload.status == 'confirmed' } ]\n\
    \x20       outputs: { orderId: $message.payload.orderId }\n";

#[test]
fn send_publishes_on_the_resolved_channel() {
    let d = describe(PLACE_AND_CONFIRM);
    let mut bus = InMemoryBroker::default();
    bus.queue(Message::correlated("orders.confirmed", "c-1", json!({"orderId": "o-1", "status": "confirmed"})));
    let out = Engine::new(&d).with_messaging(&mut bus).run("w", json!({"cid": "c-1", "product": "p"})).unwrap();
    let sent = &bus.published()[0];
    assert_eq!(sent.channel, "orders.eu", "channel parameter substituted");
    assert_eq!(sent.payload, json!({"productId": "p"}));
    assert!(sent.headers.contains(&("correlationId".into(), "c-1".into())));
    assert_eq!(out.outputs["orderId"], "o-1");
}

#[test]
fn receive_skips_messages_with_another_correlation_id() {
    let d = describe(PLACE_AND_CONFIRM);
    let mut bus = InMemoryBroker::default();
    bus.queue(Message::correlated("orders.confirmed", "other", json!({"orderId": "wrong", "status": "confirmed"})));
    bus.queue(Message::correlated("orders.confirmed", "c-1", json!({"orderId": "right", "status": "confirmed"})));
    let out = Engine::new(&d).with_messaging(&mut bus).run("w", json!({"cid": "c-1", "product": "p"})).unwrap();
    assert_eq!(out.outputs["orderId"], "right");
}

#[test]
fn receive_times_out_and_fails_the_step() {
    let d = describe(PLACE_AND_CONFIRM);
    let mut bus = InMemoryBroker::default();
    let e = Engine::new(&d).with_messaging(&mut bus).run("w", json!({"cid": "c-1", "product": "p"})).unwrap_err();
    assert!(e.to_string().contains("confirm"), "{e}");
    assert_eq!(bus.waited(), [Duration::from_millis(200)], "waited exactly the step timeout");
}

/// Without successCriteria and correlationId any message on the channel
/// within the timeout completes the step.
#[test]
fn receive_without_criteria_accepts_any_message() {
    let d = describe(
        "  - workflowId: w\n    steps:\n      - { stepId: r, operationId: $sourceDescriptions.bus.confirmOrder, action: receive, timeout: 50, outputs: { p: $message.payload } }\n",
    );
    let mut bus = InMemoryBroker::default();
    bus.queue(Message::new("orders.confirmed", json!({"any": 1})));
    let out = Engine::new(&d).with_messaging(&mut bus).run("w", json!({})).unwrap();
    assert_eq!(out.steps[0].outputs["p"], json!({"any": 1}));
}

#[test]
fn channel_path_steps() {
    let d = describe(
        "  - workflowId: w\n    steps:\n      - { stepId: r, channelPath: '{$sourceDescriptions.bus.url}#/channels/confirmations', action: receive, timeout: 50 }\n",
    );
    let mut bus = InMemoryBroker::default();
    bus.queue(Message::new("orders.confirmed", json!({})));
    Engine::new(&d).with_messaging(&mut bus).run("w", json!({})).unwrap();
}

/// The action must agree with the AsyncAPI operation it names.
#[test]
fn action_must_match_the_operation() {
    let d = describe("  - workflowId: w\n    steps: [ { stepId: s, operationId: $sourceDescriptions.bus.placeOrder, action: receive } ]\n");
    assert!(d.check().iter().any(|p| p.to_string().contains("action")));
}

/// dependsOn on a step that has not completed cannot be satisfied by a
/// sequential executor — it does not trigger the step, it fails.
#[test]
fn unsatisfied_depends_on_fails_the_step() {
    let d = describe(
        "  - workflowId: w\n    steps:\n\
         \x20     - { stepId: place, operationId: $sourceDescriptions.bus.placeOrder, action: send, onSuccess: [ { name: skip, type: goto, stepId: confirm } ] }\n\
         \x20     - { stepId: never, operationId: $sourceDescriptions.bus.placeOrder, action: send }\n\
         \x20     - { stepId: confirm, operationId: $sourceDescriptions.bus.confirmOrder, action: receive, timeout: 10, dependsOn: [ never ] }\n",
    );
    let mut bus = InMemoryBroker::default();
    let e = Engine::new(&d).with_messaging(&mut bus).run("w", json!({})).unwrap_err();
    assert!(e.to_string().contains("never"), "{e}");
}

#[test]
fn depends_on_across_workflows_in_a_session() {
    let d = describe(
        "  - workflowId: producer\n    steps:\n\
         \x20     - { stepId: wait, operationId: $sourceDescriptions.bus.confirmOrder, action: receive, timeout: 50, outputs: { id: $message.payload.id } }\n\
         \x20 - workflowId: consumer\n    steps:\n\
         \x20     - stepId: use\n        operationId: $sourceDescriptions.bus.placeOrder\n        action: send\n\
         \x20       dependsOn: [ $workflows.producer.steps.wait ]\n\
         \x20       parameters: [ { name: region, in: channel, value: $workflows.producer.steps.wait.outputs.id } ]\n",
    );
    let mut bus = InMemoryBroker::default();
    let e = Engine::new(&d).with_messaging(&mut bus).run("consumer", json!({})).unwrap_err();
    assert!(e.to_string().contains("producer"), "dependency not satisfied before producer: {e}");

    let mut bus = InMemoryBroker::default();
    bus.queue(Message::new("orders.confirmed", json!({"id": "us"})));
    let mut session = Engine::new(&d).with_messaging(&mut bus).session();
    session.run("producer", json!({})).unwrap();
    session.run("consumer", json!({})).unwrap();
    assert_eq!(bus.published()[0].channel, "orders.us");
}

/// A live broker keeps a message only for a channel someone already
/// subscribed to. The reply to `place` is published the moment the order is
/// sent — before `confirm` runs — so the engine must subscribe to every
/// receive channel before the first step, or the reply is lost.
#[test]
fn receive_channels_are_subscribed_before_the_first_step() {
    let d = describe(PLACE_AND_CONFIRM);
    let mut bus = InMemoryBroker::default();
    bus.reply_on("orders.eu", Message::correlated("orders.confirmed", "c-1", json!({"status": "confirmed", "orderId": "o-1"})));
    let out = Engine::new(&d).with_messaging(&mut bus).run("w", json!({"cid": "c-1", "product": "p"})).unwrap();
    assert_eq!(out.outputs["orderId"], "o-1");
    assert_eq!(bus.subscribed(), ["orders.confirmed"]);
}

/// The broker itself: without a subscription a live reply is dropped.
#[test]
fn a_live_reply_without_subscription_is_lost() {
    use arazzo_jrpc::transport::MessageTransport;
    let mut bus = InMemoryBroker::default();
    bus.reply_on("orders.eu", Message::new("orders.confirmed", json!({})));
    bus.send(&Message::new("orders.eu", json!({}))).unwrap();
    assert_eq!(bus.receive("orders.confirmed", None, Duration::from_millis(5)).unwrap(), None);
    bus.subscribe("orders.{region}").unwrap();
    bus.reply_on("orders.eu", Message::new("orders.us", json!({"n": 1})));
    bus.send(&Message::new("orders.eu", json!({}))).unwrap();
    assert!(bus.receive("orders.us", None, Duration::from_millis(5)).unwrap().is_some(), "channel template covers the address");
}

#[test]
fn depends_on_must_name_existing_steps() {
    let mut s = Sources::new();
    s.insert(
        ENTRY,
        "arazzo: 1.1.0\ninfo: { title: t, version: '1' }\n\
         sourceDescriptions: [ { name: bus, url: ../specs/orders.asyncapi.yaml, type: asyncapi } ]\n\
         workflows: [ { workflowId: w, steps: [ { stepId: a, operationId: $sourceDescriptions.bus.placeOrder, action: send, dependsOn: [ ghost ] } ] } ]\n",
    )
    .unwrap();
    s.insert(BUS, &asyncapi()).unwrap();
    let e = Description::load(&s, ENTRY).unwrap_err();
    assert!(e.to_string().contains("ghost"), "{e}");
}
