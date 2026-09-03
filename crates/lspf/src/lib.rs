//! lspf — a Rust framework for building extensible LSP language servers.
//!
//! See `CONTEXT.md` and `docs/adr/` at the repository root for the domain
//! language and the architectural decisions that shape this crate.
//!
//! The public protocol surface follows the stable LSP 3.18 specification.

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

#[cfg(all(target_arch = "wasm32", feature = "testing"))]
compile_error!("the `testing` feature requires a native target");

mod builder;
mod capability;
mod client;
#[cfg(any(feature = "runtime-tokio", target_arch = "wasm32"))]
mod client_endpoint;
#[cfg(any(feature = "runtime-tokio", target_arch = "wasm32"))]
mod client_progress;
mod codec;
mod context;
mod documents;
mod notebooks;
// Serving needs an executor: the engine exists wherever a runtime is
// available (ADR 0020). On native targets that is the `runtime-tokio`
// feature; on wasm32 the `wasm` feature, which the check above enforces.
#[cfg(any(feature = "runtime-tokio", target_arch = "wasm32"))]
mod engine;
mod error;
mod failure;
pub mod features;
mod file_provider;
#[cfg(all(feature = "fuzzing", not(target_arch = "wasm32")))]
#[doc(hidden)]
pub mod fuzzing;
mod partial_result;
mod progress;
mod raw;
mod resource_policy;
mod runtime;
mod service;
#[cfg(any(feature = "runtime-tokio", target_arch = "wasm32"))]
mod session;
mod sync;
mod telemetry;
#[cfg(all(feature = "testing", not(target_arch = "wasm32")))]
pub mod testing;
mod transport;
mod uri_key;
mod workspace;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod test_util;

pub mod types {
    //! LSP protocol types — re-exported from `gen-lsp-types` per ADR 0032.
    pub use gen_lsp_types::*;
    pub use gen_lsp_types::{
        ApplyWorkspaceEditResult as ApplyWorkspaceEditResponse,
        CodeActionResponse as CodeActionOrCommand, Contents as HoverContents,
        DefinitionParams as GotoDefinitionParams, DefinitionResponse as GotoDefinitionResponse,
        DiagnosticProvider as DiagnosticServerCapabilities,
        DocumentColorOptions as ColorProviderOptions,
        DocumentDiagnosticReport as DocumentDiagnosticReportResult, Label as InlayHintLabel,
        PrepareRenameResult as PrepareRenameResponse, ReferenceOptions as ReferencesOptions,
        Save as TextDocumentSyncSaveOptions, SemanticTokenModifiers as SemanticTokenModifier,
        SemanticTokenTypes as SemanticTokenType, SemanticTokens as SemanticTokensRangeResult,
        SemanticTokens as SemanticTokensResult,
        SemanticTokensDeltaResponse as SemanticTokensFullDeltaResult,
        TextDocumentSync as TextDocumentSyncCapability, Tooltip as InlayHintTooltip,
        WorkspaceDiagnosticReport as WorkspaceDiagnosticReportResult,
        WorkspaceOptions as WorkspaceServerCapabilities,
    };

    /// Request markers and the custom-request trait compatibility boundary.
    pub mod request {
        use serde::Serialize;
        use serde::de::DeserializeOwned;

        /// A typed LSP request marker.
        pub trait Request {
            /// Request parameters.
            type Params: DeserializeOwned + Serialize + Send + Sync + 'static;
            /// Successful response value.
            type Result: DeserializeOwned + Serialize + Send + Sync + 'static;
            /// JSON-RPC method name.
            const METHOD: &'static str;
        }

        impl<T: gen_lsp_types::Request> Request for T {
            type Params = T::Params;
            type Result = T::Result;
            const METHOD: &'static str = T::METHOD.as_str();
        }

        pub use gen_lsp_types::{
            ApplyWorkspaceEditRequest as ApplyWorkspaceEdit,
            CallHierarchyIncomingCallsRequest as CallHierarchyIncomingCalls,
            CallHierarchyOutgoingCallsRequest as CallHierarchyOutgoingCalls,
            CallHierarchyPrepareRequest as CallHierarchyPrepare, CodeActionRequest,
            CodeActionResolveRequest, CodeLensRefreshRequest as CodeLensRefresh, CodeLensRequest,
            CodeLensResolveRequest as CodeLensResolve, ColorPresentationRequest,
            CompletionRequest as Completion, CompletionResolveRequest as ResolveCompletionItem,
            ConfigurationRequest as WorkspaceConfiguration,
            DeclarationParams as GotoDeclarationParams, DeclarationRequest as GotoDeclaration,
            DeclarationResponse as GotoDeclarationResponse, DefinitionRequest as GotoDefinition,
            DiagnosticRefreshRequest as WorkspaceDiagnosticRefresh,
            DocumentColorRequest as DocumentColor, DocumentDiagnosticRequest,
            DocumentFormattingRequest as Formatting, DocumentHighlightRequest, DocumentLinkRequest,
            DocumentLinkResolveRequest as DocumentLinkResolve,
            DocumentOnTypeFormattingRequest as OnTypeFormatting,
            DocumentRangeFormattingRequest as RangeFormatting, DocumentRangesFormattingRequest,
            DocumentSymbolRequest, ExecuteCommandRequest as ExecuteCommand,
            FoldingRangeRefreshRequest, FoldingRangeRequest, HoverRequest,
            ImplementationParams as GotoImplementationParams,
            ImplementationRequest as GotoImplementation,
            ImplementationResponse as GotoImplementationResponse, InlayHintRefreshRequest,
            InlayHintRequest, InlayHintResolveRequest, InlineCompletionRequest,
            InlineValueRefreshRequest, InlineValueRequest,
            LinkedEditingRangeRequest as LinkedEditingRange, MonikerRequest, PrepareRenameRequest,
            ReferencesRequest as References, RegistrationRequest as RegisterCapability,
            RenameRequest as Rename, SelectionRangeRequest,
            SemanticTokensDeltaRequest as SemanticTokensFullDeltaRequest,
            SemanticTokensRangeRequest, SemanticTokensRefreshRequest as SemanticTokensRefresh,
            SemanticTokensRequest as SemanticTokensFullRequest,
            ShowDocumentRequest as ShowDocument, ShowMessageRequest, ShutdownRequest as Shutdown,
            SignatureHelpRequest, TextDocumentContentRefreshRequest, TextDocumentContentRequest,
            TypeDefinitionParams as GotoTypeDefinitionParams,
            TypeDefinitionRequest as GotoTypeDefinition,
            TypeDefinitionResponse as GotoTypeDefinitionResponse,
            TypeHierarchyPrepareRequest as TypeHierarchyPrepare,
            TypeHierarchySubtypesRequest as TypeHierarchySubtypes,
            TypeHierarchySupertypesRequest as TypeHierarchySupertypes,
            UnregistrationRequest as UnregisterCapability,
            WillCreateFilesRequest as WillCreateFiles, WillDeleteFilesRequest as WillDeleteFiles,
            WillRenameFilesRequest as WillRenameFiles,
            WillSaveTextDocumentWaitUntilRequest as WillSaveWaitUntil,
            WorkDoneProgressCreateRequest as WorkDoneProgressCreate, WorkspaceDiagnosticRequest,
            WorkspaceFoldersRequest, WorkspaceSymbolRequest,
            WorkspaceSymbolResolveRequest as WorkspaceSymbolResolve,
        };
    }

    /// Notification markers and the custom-notification trait compatibility boundary.
    pub mod notification {
        use serde::Serialize;
        use serde::de::DeserializeOwned;

        /// A typed LSP notification marker.
        pub trait Notification {
            /// Notification parameters.
            type Params: DeserializeOwned + Serialize + Send + Sync + 'static;
            /// JSON-RPC method name.
            const METHOD: &'static str;
        }

        impl<T: gen_lsp_types::Notification> Notification for T {
            type Params = T::Params;
            const METHOD: &'static str = T::METHOD.as_str();
        }

        pub use gen_lsp_types::{
            CancelNotification as Cancel,
            DidChangeConfigurationNotification as DidChangeConfiguration,
            DidChangeNotebookDocumentNotification as DidChangeNotebookDocument,
            DidChangeTextDocumentNotification as DidChangeTextDocument,
            DidChangeWatchedFilesNotification as DidChangeWatchedFiles,
            DidChangeWorkspaceFoldersNotification as DidChangeWorkspaceFolders,
            DidCloseNotebookDocumentNotification as DidCloseNotebookDocument,
            DidCloseTextDocumentNotification as DidCloseTextDocument,
            DidCreateFilesNotification as DidCreateFiles,
            DidDeleteFilesNotification as DidDeleteFiles,
            DidOpenNotebookDocumentNotification as DidOpenNotebookDocument,
            DidOpenTextDocumentNotification as DidOpenTextDocument,
            DidRenameFilesNotification as DidRenameFiles,
            DidSaveNotebookDocumentNotification as DidSaveNotebookDocument,
            DidSaveTextDocumentNotification as DidSaveTextDocument, ExitNotification as Exit,
            InitializedNotification as Initialized, LogMessageNotification as LogMessage,
            LogTraceNotification as LogTrace, ProgressNotification as Progress,
            PublishDiagnosticsNotification as PublishDiagnostics, SetTraceNotification as SetTrace,
            ShowMessageNotification as ShowMessage, TelemetryEventNotification as TelemetryEvent,
            WillSaveTextDocumentNotification as WillSaveTextDocument,
            WorkDoneProgressCancelNotification as WorkDoneProgressCancel,
        };
    }
}

/// Compiles the Rust in the repository's user-facing Markdown as doc-tests, so
/// a quickstart or a guide example cannot drift from the surface it documents.
/// The module exists only under `cargo test --doc`; nothing here is part of
/// the public API.
#[cfg(doctest)]
mod markdown {
    #[doc = include_str!("../../../README.md")]
    pub struct Readme;

    #[doc = include_str!("../../../docs/guides/features-and-workspace.md")]
    pub struct FeaturesAndWorkspaceGuide;

    #[doc = include_str!("../../../docs/guides/outgoing-client.md")]
    pub struct OutgoingClientGuide;

    #[doc = include_str!("../../../docs/guides/client-adoption.md")]
    pub struct ClientAdoptionGuide;

    #[doc = include_str!("../../../docs/guides/transports.md")]
    pub struct TransportsGuide;

    #[doc = include_str!("../../../docs/guides/testing.md")]
    pub struct TestingGuide;

    #[doc = include_str!("../../../docs/guides/errors-and-cancellation.md")]
    pub struct ErrorsAndCancellationGuide;

    #[doc = include_str!("../../../docs/guides/operations.md")]
    pub struct OperationsGuide;

    #[doc = include_str!("../../../docs/tutorials/server.md")]
    pub struct ServerTutorial;

    #[doc = include_str!("../../../docs/tutorials/client.md")]
    pub struct ClientTutorial;
}

pub use builder::SharedHandler;
pub use builder::{InitializeRegistrar, Server, ServerBuilder};
pub use client::{ClientHandle, TelemetryEventParams};
#[cfg(any(feature = "runtime-tokio", target_arch = "wasm32"))]
pub use client_endpoint::{Client, ClientBuilder, ClientConnection, ClientContext, ServerHandle};
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
pub use notebooks::{Notebook, NotebooksView};
pub use partial_result::{PartialResultRequest, PartialResultSink};
pub use progress::{ProgressHandle, ProgressOptions};
pub use raw::{JsonRpcError, RawMessage, RequestId};
pub use resource_policy::{ResourcePolicy, ResourcePolicyField};
pub use runtime::{TaskFuture, TaskSend};
pub use service::{CallKind, IncomingCall, Layer, Next, ServiceFuture, ServiceResult};
#[cfg(all(feature = "stdio", not(target_arch = "wasm32")))]
pub use transport::{
    ChildConnection, ChildError, ChildOutput, StdioBuilder, StdioReader, StdioTransport,
    StdioWriter, stdio,
};
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
