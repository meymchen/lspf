//! lspf — a Rust framework for building extensible LSP language servers.
//!
//! See `CONTEXT.md` and `docs/adr/` at the repository root for the domain
//! language and the architectural decisions that shape this crate.
//!
//! The default documentation is the stable LSP 3.17 surface. APIs in the
//! `proposed` module and proposed-only [`ClientHandle`] methods are explicitly
//! marked unstable and appear only when the `proposed` feature is enabled.

#![deny(missing_docs)]
// A protocol-only feature row intentionally has no serving engine. The
// public registration and protocol types remain useful there, while the
// engine-owned internals are necessarily dormant until a Transport selects a
// runtime. All other warnings stay denied in CI for this row.
#![cfg_attr(
    all(not(target_arch = "wasm32"), not(feature = "runtime-tokio")),
    allow(dead_code)
)]

#[cfg(all(target_arch = "wasm32", not(feature = "wasm")))]
compile_error!(
    "the wasm32 target requires the `wasm` feature; \
     build with `--no-default-features --features worker-channel`, \
     or `--no-default-features --features wasm` plus your transports"
);

// The worker-channel adapter wraps a browser or Node Worker MessagePort: it
// only exists inside a Worker on `wasm32-unknown-unknown`.
#[cfg(all(feature = "worker-channel", not(target_arch = "wasm32")))]
compile_error!("the `worker-channel` feature requires the wasm32 target");

// Native socket adapters depend on Tokio's reactor and are deliberately not
// part of the Worker-hosted WASM surface. Keep these diagnostics here, next to
// target/feature contract, so an unsupported feature combination fails for a
// reason that tells the caller how to fix it.
#[cfg(all(target_arch = "wasm32", feature = "tcp"))]
compile_error!("the `tcp` feature is not supported on the wasm32 target");

#[cfg(all(target_arch = "wasm32", feature = "websocket"))]
compile_error!("the `websocket` feature is not supported on the wasm32 target");

mod builder;
mod capability;
mod client;
mod codec;
mod context;
mod documents;
// Serving needs an executor: the engine exists wherever a runtime is
// available (ADR 0020). On native targets that is the `runtime-tokio`
// feature; on wasm32 the `wasm` feature, which the check above enforces.
#[cfg(any(feature = "runtime-tokio", target_arch = "wasm32"))]
mod engine;
mod error;
mod failure;
pub mod features;
mod file_provider;
mod progress;
#[cfg(feature = "proposed")]
pub mod proposed;
mod raw;
mod resource_policy;
mod runtime;
mod service;
mod sync;
mod telemetry;
mod transport;
mod uri_key;
mod workspace;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod test_util;

pub mod types {
    //! LSP protocol types — re-exported from `lsp-types` per ADR 0014.
    pub use lsp_types::*;
}

/// Compiles the Rust in the repository's user-facing Markdown as doc-tests, so
/// a quickstart or a guide example cannot drift from the surface it documents.
/// The module exists only under `cargo test --doc`; nothing here is part of
/// the public API.
#[cfg(doctest)]
mod markdown {
    #[doc = include_str!("../../../README.md")]
    pub struct Readme;

    #[doc = include_str!("../../../README.zh-CN.md")]
    pub struct ReadmeZhCn;

    #[doc = include_str!("../../../docs/guides/features-and-workspace.md")]
    pub struct FeaturesAndWorkspaceGuide;

    #[doc = include_str!("../../../docs/guides/features-and-workspace.zh-CN.md")]
    pub struct FeaturesAndWorkspaceGuideZhCn;

    #[doc = include_str!("../../../docs/guides/outgoing-client.md")]
    pub struct OutgoingClientGuide;

    #[doc = include_str!("../../../docs/guides/outgoing-client.zh-CN.md")]
    pub struct OutgoingClientGuideZhCn;

    #[doc = include_str!("../../../docs/guides/transports.md")]
    pub struct TransportsGuide;

    #[doc = include_str!("../../../docs/guides/transports.zh-CN.md")]
    pub struct TransportsGuideZhCn;
}

#[doc(hidden)]
pub use builder::SharedHandler;
pub use builder::{InitializeRegistrar, Server, ServerBuilder};
pub use client::{ClientHandle, TelemetryEventParams};
pub use context::ServerContext;
pub use documents::{Document, DocumentsView, PositionEncoding};
#[cfg(any(feature = "runtime-tokio", target_arch = "wasm32"))]
pub use engine::Outcome;
pub use error::{BuildError, ClientError, Error, LspError, ProgressError, Result};
pub use failure::{
    ConnectionDirection, ConnectionFailure, ConnectionFailureCategory, ConnectionFailureContext,
    ConnectionRequestId,
};
pub use features::{FeatureSpec, NotificationFeatureSpec};
#[cfg(target_arch = "wasm32")]
pub use file_provider::EmptyFileProvider;
pub use file_provider::{FileProvider, MemoryFileProvider};
#[cfg(all(not(target_arch = "wasm32"), feature = "runtime-tokio"))]
pub use file_provider::{OsFileProvider, OsFileProviderBuilder};
pub use progress::{ProgressHandle, ProgressOptions};
pub use raw::{JsonRpcError, RawMessage, RequestId};
pub use resource_policy::{ResourcePolicy, ResourcePolicyField};
#[doc(hidden)]
pub use runtime::{TaskFuture, TaskSend};
pub use service::{CallKind, IncomingCall, Layer, Next, ServiceFuture, ServiceResult};
#[cfg(all(feature = "stdio", not(target_arch = "wasm32")))]
pub use transport::{StdioBuilder, StdioReader, StdioTransport, StdioWriter, stdio};
#[cfg(all(feature = "tcp", not(target_arch = "wasm32")))]
pub use transport::{TcpBuilder, TcpReader, TcpTransport, TcpWriter, tcp};
pub use transport::{Transport, TransportError, TransportReader, TransportWriter};
#[cfg(all(feature = "websocket", not(target_arch = "wasm32")))]
pub use transport::{
    WebSocketBuilder, WebSocketReader, WebSocketTransport, WebSocketWriter, websocket,
};
#[cfg(all(feature = "worker-channel", target_arch = "wasm32"))]
pub use transport::{
    WorkerChannelBuilder, WorkerChannelReader, WorkerChannelTransport, WorkerChannelWriter,
    worker_channel,
};
pub use workspace::{Workspace, WorkspaceError};

/// Cancellation primitive passed to every request handler (ADR 0007).
pub use tokio_util::sync::CancellationToken;

/// Cap on calls in flight inside the user Layer chain when a [`Server`] does
/// not set its own with [`ServerBuilder::concurrency_limit`] (ADR 0012).
///
/// The cap is connection policy, so it belongs to the built [`Server`] that
/// owns the connection rather than to the `Transport` it is served over.
pub const DEFAULT_CONCURRENCY_LIMIT: usize = 64;

/// Historical name for the default outbound-message budget. New code should
/// configure [`ResourcePolicy::max_outbound_messages`] through
/// [`ServerBuilder::resource_policy`].
pub const DEFAULT_OUTBOUND_WARNING_THRESHOLD: usize = 1024;
