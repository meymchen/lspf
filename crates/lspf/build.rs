use std::env;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

const META_MODEL: &str = "tests/fixtures/lsp_meta_model_3_18_0.json";

fn main() {
    println!("cargo:rerun-if-changed={META_MODEL}");

    let model: Value = serde_json::from_str(
        &fs::read_to_string(META_MODEL).expect("read the vendored LSP metaModel fixture"),
    )
    .expect("parse the vendored LSP metaModel fixture");
    let requests = model["requests"]
        .as_array()
        .expect("the vendored metaModel lists requests");

    let mut implementations = String::new();
    let mut methods = Vec::new();
    for request in requests
        .iter()
        .filter(|request| !request["partialResult"].is_null())
    {
        let marker = request["typeName"]
            .as_str()
            .expect("a partial-result request has a typeName");
        assert!(
            marker
                .chars()
                .all(|character| character.is_ascii_alphanumeric()),
            "request marker is a Rust identifier: {marker}"
        );
        let method = request["method"]
            .as_str()
            .expect("a partial-result request has a method");
        implementations.push_str(&format!(
            "impl sealed::Sealed for gen_lsp_types::{marker} {{}}\n\
             impl PartialResultRequest for gen_lsp_types::{marker} {{}}\n"
        ));
        methods.push(method);
    }

    implementations.push_str(
        "\npub(crate) fn supports_method(method: &str) -> bool {\n    matches!(method,\n",
    );
    implementations.push_str(
        &methods
            .into_iter()
            .map(|method| format!("        {method:?}"))
            .collect::<Vec<_>>()
            .join(" |\n"),
    );
    implementations.push_str("\n    )\n}\n");

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("partial_result_requests.rs");
    fs::write(output, implementations).expect("write generated partial-result request catalog");
}
