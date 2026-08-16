//! The shared handler example (ADR 0020): one handler source compiles and
//! runs on both execution models.
//!
//! On native targets the same `Server` serves over stdio on `TokioRuntime`;
//! the identical registrations compile for `wasm32-unknown-unknown` as well,
//! where the browser host owns the connection and the framework runs the
//! handlers on `WasmRuntime`. No registration method, parameter, or return
//! shape forks between the two targets — only the internal task bounds
//! differ, expressed through the hidden `TaskSend` marker.
//!
//! ```text
//! cargo check -p lspf --example shared_server
//! cargo check -p lspf --example shared_server \
//!   --target wasm32-unknown-unknown --no-default-features --features wasm
//! ```

use std::sync::Arc;

use lspf::types::notification::DidOpenTextDocument;
use lspf::types::{
    CompletionItem, CompletionOptions, CompletionParams, CompletionResponse,
    DidOpenTextDocumentParams, Hover, HoverParams,
};
use lspf::{CancellationToken, Context, LspError, Server};

/// The example server's shared application state.
struct State {
    label: String,
}

impl State {
    fn new() -> Self {
        Self {
            label: "shared".to_string(),
        }
    }
}

/// The typed `textDocument/hover` feature.
async fn hover(
    state: Arc<State>,
    ctx: Context,
    _params: HoverParams,
    _ct: CancellationToken,
) -> Result<Option<Hover>, LspError> {
    let _ = ctx.workspace().roots();
    Ok(Some(Hover {
        contents: lspf::types::HoverContents::Scalar(lspf::types::MarkedString::String(
            state.label.clone(),
        )),
        range: None,
    }))
}

/// The typed `textDocument/completion` feature.
async fn completion(
    _state: Arc<State>,
    _ctx: Context,
    _params: CompletionParams,
    _ct: CancellationToken,
) -> Result<Option<CompletionResponse>, LspError> {
    Ok(Some(CompletionResponse::Array(vec![CompletionItem {
        label: "shared".to_string(),
        ..CompletionItem::default()
    }])))
}

/// A typed custom request marker: same method, params, and result on both
/// targets.
enum SharedPing {}

impl lspf::types::request::Request for SharedPing {
    type Params = String;
    type Result = String;
    const METHOD: &'static str = "shared/ping";
}

async fn ping(
    state: Arc<State>,
    _ctx: Context,
    params: String,
    _ct: CancellationToken,
) -> Result<String, LspError> {
    Ok(format!("{}:{}", state.label, params))
}

/// The post-mutation hook for the built-in `textDocument/didOpen`.
async fn on_did_open(_state: Arc<State>, _ctx: Context, _params: DidOpenTextDocumentParams) {}

/// The one shared registration surface. Both binaries below hand the same
/// `Server` to their target's serving path.
fn build() -> std::result::Result<Server<State>, lspf::BuildError> {
    Server::builder(State::new())
        .feature(lspf::features::hover(), hover)
        .feature(
            lspf::features::completion(CompletionOptions::default()),
            completion,
        )
        .request::<SharedPing, _, _>(ping)
        .notification::<DidOpenTextDocument, _, _>(on_did_open)
        .build()
}

// `TaskSend` carries the whole target-dependent mobility difference: `Send`
// on native, nothing on wasm32. These assertions are compile-time evidence
// that the one registration source satisfies both targets' bounds.
const _: fn() = || {
    fn assert_task_send<T: lspf::TaskSend>() {}
    assert_task_send::<&str>();
    #[cfg(target_arch = "wasm32")]
    assert_task_send::<std::rc::Rc<()>>();
};

fn main() {
    let server = build().expect("the static registrations are valid");

    // Native: serve over the stdio transport on the caller's Tokio runtime.
    #[cfg(not(target_arch = "wasm32"))]
    serve_native(server);

    // WASM: the browser host drives `Server::serve` over its worker-channel
    // transport; an example binary has no host, so it just drops the server.
    #[cfg(target_arch = "wasm32")]
    drop(server);
}

#[cfg(not(target_arch = "wasm32"))]
fn serve_native(server: Server<State>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a Tokio runtime starts");
    let outcome = runtime
        .block_on(lspf::stdio(server).serve())
        .expect("serving ends without a transport error");
    std::process::exit(outcome.code());
}
