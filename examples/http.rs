//! Run the bundled `examples/data/` workflow against https://httpbin.org
//! with the built-in `ReqwestHttp` transport (feature `http`).
//!
//! ```sh
//! cargo run --example http -- hello 204
//! ```

use std::path::Path;

use arazzo_jrpc::transport::http::ReqwestHttp;
use arazzo_jrpc::{Description, Done, Engine, Sources};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let q = args.next().unwrap_or_else(|| "hello".into());
    let code: u16 = args.next().map(|c| c.parse()).transpose()?.unwrap_or(204);

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/data");
    let mut sources = Sources::new();
    for name in ["httpbin.arazzo.yaml", "httpbin.openapi.yaml"] {
        let path = dir.join(name);
        let uri = url::Url::from_file_path(&path).expect("absolute path").to_string();
        sources.insert(&uri, &std::fs::read_to_string(&path)?)?;
    }
    let entry = url::Url::from_file_path(dir.join("httpbin.arazzo.yaml")).expect("absolute path").to_string();
    let description = Description::load(&sources, &entry)?;
    let problems = description.check();
    if !problems.is_empty() {
        return Err(format!("description has problems: {problems:?}").into());
    }

    let mut http = ReqwestHttp::default();
    let mut report = |d: &Done| match &d.failed {
        None => println!("step {} ({}) ok, outputs {}", d.step_id, d.operation, json!(d.outputs)),
        Some(why) => println!("step {} ({}) failed: {why}", d.step_id, d.operation),
    };
    let out = Engine::new(&description)
        .with_http(&mut http)
        .on_step(&mut report)
        .run("roundTrip", json!({ "q": q, "code": code }))?;
    println!("workflow outputs: {}", json!(out.outputs));
    Ok(())
}
