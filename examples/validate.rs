//! Load an Arazzo description from files and print what `check()` finds.
//!
//! ```sh
//! cargo run --example validate -- flow.arazzo.yaml api.openapi.yaml
//! ```
//!
//! The first file is the entry document; the rest are the API documents it
//! refers to. Without arguments the bundled `examples/data/` pair is used.

use std::path::Path;

use arazzo_jrpc::{Description, Sources};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/data");
    let files: Vec<String> = if args.is_empty() {
        ["httpbin.arazzo.yaml", "httpbin.openapi.yaml"].iter().map(|f| dir.join(f).to_string_lossy().into_owned()).collect()
    } else {
        args
    };

    let mut sources = Sources::new();
    let mut entry = None;
    for file in &files {
        let uri = url::Url::from_file_path(Path::new(file).canonicalize()?).expect("absolute path").to_string();
        sources.insert(&uri, &std::fs::read_to_string(file)?)?;
        entry.get_or_insert(uri);
    }
    let description = Description::load(&sources, &entry.expect("at least one file"))?;

    let problems = description.check();
    for w in &description.entry().workflows {
        println!("workflow {} ({} steps)", w.workflow_id, w.steps.len());
    }
    if problems.is_empty() {
        println!("ok: no problems");
        return Ok(());
    }
    for p in &problems {
        println!("problem: {p}");
    }
    std::process::exit(1)
}
