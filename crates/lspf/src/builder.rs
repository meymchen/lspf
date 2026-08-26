//! The connection-owned builder surface (ADR 0017, ADR 0018).
//!
//! [`Server::builder`] collects static registrations against one application
//! state value; [`ServerBuilder::build`] validates them and returns a [`Server`]
//! without performing any I/O or freezing the [`Router`]. The protocol engine
//! freezes the Router later, when it commits the initialize transaction: after a
//! valid `initialize`, it runs the sole [`configure_initialize`] callback
//! against a transactional [`InitializeRegistrar`], then the [`on_initialize`]
//! lifecycle hook. This surface wires typed custom requests and notifications,
//! typed commands beneath `workspace/executeCommand`, the standard features
//! with sealed descriptors in [`lspf::features`](crate::features), and the
//! lifecycle hooks: [`on_initialize`], [`on_initialized`] (which runs once the
//! client acknowledges initialization), [`on_shutdown`] (which gates the
//! transition into shutting down), and [`on_exit`] (which observes the
//! connection's ending without being able to change its [`Outcome`]).
//!
//! [`configure_initialize`]: ServerBuilder::configure_initialize
//! [`on_initialize`]: ServerBuilder::on_initialize
//! [`on_initialized`]: ServerBuilder::on_initialized
//! [`on_shutdown`]: ServerBuilder::on_shutdown
//! [`on_exit`]: ServerBuilder::on_exit
//! [`on_error`]: ServerBuilder::on_error

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[cfg(test)]
use lsp_types::ServerCapabilities;
use lsp_types::notification::Notification;
use lsp_types::request::Request;
use lsp_types::{
    InitializeParams, InitializedParams, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::FileProvider;
use crate::capability::{CapabilityBuilder, GeneratedCapabilities};
use crate::codec::erase_value;
use crate::context::Context;
use crate::error::{BuildError, LspError};
use crate::features::{FeatureSpec, NotificationFeatureSpec};
use crate::file_provider::{SharedFileProvider, erase};
use crate::runtime::{TaskFuture, TaskSend};
use crate::service::{Layer, UserLayer};

/// Method names owned by the framework's lifecycle; a custom request or
/// notification may not shadow one of them.
const RESERVED_METHODS: &[&str] = &[
    "initialize",
    "shutdown",
    "exit",
    "initialized",
    "$/cancelRequest",
];

/// The wire method commands dispatch beneath. A command registration and an
/// explicit request handler for this method cannot coexist.
const EXECUTE_COMMAND_METHOD: &str = "workspace/executeCommand";

/// A notification whose validation or state mutation the protocol engine owns.
///
/// A `notification` registration for one of these methods records the
/// connection's single post-validation hook rather than a Router route. When
/// the notification mutates protocol state, the hook observes that mutation
/// instead of replacing it. This enum is the one place that says which methods
/// those are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolNotification {
    Open,
    Change,
    Close,
    WillSave,
    Save,
    WorkspaceFolders,
    Configuration,
    Trace,
    /// `window/workDoneProgress/cancel`: the engine fires the matching
    /// handle's cancellation token; a registration records the
    /// post-validation hook.
    ProgressCancel,
}

impl ProtocolNotification {
    const OPEN_METHOD: &'static str = "textDocument/didOpen";
    const CHANGE_METHOD: &'static str = "textDocument/didChange";
    const CLOSE_METHOD: &'static str = "textDocument/didClose";
    const WILL_SAVE_METHOD: &'static str = "textDocument/willSave";
    const SAVE_METHOD: &'static str = "textDocument/didSave";

    /// The built-in this wire method names, or `None` when the method is an
    /// ordinary route.
    pub(crate) fn from_method(method: &str) -> Option<Self> {
        match method {
            Self::OPEN_METHOD => Some(Self::Open),
            Self::CHANGE_METHOD => Some(Self::Change),
            Self::CLOSE_METHOD => Some(Self::Close),
            Self::WILL_SAVE_METHOD => Some(Self::WillSave),
            Self::SAVE_METHOD => Some(Self::Save),
            "workspace/didChangeWorkspaceFolders" => Some(Self::WorkspaceFolders),
            "workspace/didChangeConfiguration" => Some(Self::Configuration),
            "$/setTrace" => Some(Self::Trace),
            "window/workDoneProgress/cancel" => Some(Self::ProgressCancel),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentSyncSettings {
    pub(crate) capability: TextDocumentSyncCapability,
    pub(crate) options: TextDocumentSyncOptions,
}

/// The future produced by an erased request or command handler: its decoded,
/// method-erased result or the error to report.
type HandlerFuture = Pin<Box<dyn TaskFuture<Result<Value, LspError>>>>;

/// The future produced by an erased notification handler. A notification has no
/// response, so it resolves to `()`; when decoding fails the future logs the
/// error and returns without invoking the typed handler.
type NotificationFuture = Pin<Box<dyn TaskFuture<()>>>;

/// One target-aware erasure boundary for handlers shared by runtime tasks.
///
/// On native targets a stored handler must also be `Sync` because multiple
/// Tokio tasks can invoke it through shared framework state. In a Web Worker
/// all invocations stay on one thread. Futures retain their separate mobility
/// requirement through [`TaskFuture`].
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub trait SharedHandler<Args, Output>: TaskSend + Sync {
    fn invoke(&self, args: Args) -> Output;
}

#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub trait SharedHandler<Args, Output>: TaskSend {
    fn invoke(&self, args: Args) -> Output;
}

macro_rules! impl_shared_handler {
    ($($arg:ident),+) => {
        #[cfg(not(target_arch = "wasm32"))]
        impl<F, Output, $($arg),+> SharedHandler<($($arg,)+), Output> for F
        where
            F: Fn($($arg),+) -> Output + TaskSend + Sync,
        {
            #[allow(non_snake_case)]
            fn invoke(&self, ($($arg,)+): ($($arg,)+)) -> Output {
                self($($arg),+)
            }
        }

        #[cfg(target_arch = "wasm32")]
        impl<F, Output, $($arg),+> SharedHandler<($($arg,)+), Output> for F
        where
            F: Fn($($arg),+) -> Output + TaskSend,
        {
            #[allow(non_snake_case)]
            fn invoke(&self, ($($arg,)+): ($($arg,)+)) -> Output {
                self($($arg),+)
            }
        }
    };
}

impl_shared_handler!(A, B);
impl_shared_handler!(A);
impl_shared_handler!(A, B, C);
impl_shared_handler!(A, B, C, D);

pub(crate) trait ConfigureInitializeCallback<S>: TaskSend {
    fn invoke(
        self: Box<Self>,
        params: &InitializeParams,
        registrar: &mut InitializeRegistrar<S>,
    ) -> Result<(), LspError>;
}

impl<S, F> ConfigureInitializeCallback<S> for F
where
    F: FnOnce(&InitializeParams, &mut InitializeRegistrar<S>) -> Result<(), LspError> + TaskSend,
{
    fn invoke(
        self: Box<Self>,
        params: &InitializeParams,
        registrar: &mut InitializeRegistrar<S>,
    ) -> Result<(), LspError> {
        self(params, registrar)
    }
}

/// A type-erased custom request handler stored in the frozen [`Router`].
///
/// Its three responsibilities (ADR 0017) are to decode the incoming
/// parameters once, invoke the typed handler with native values, and encode
/// the success value once. Malformed parameters become
/// [`LspError::InvalidParams`] without ever calling the typed handler.
pub(crate) type ErasedRequestHandler<S> =
    Box<dyn SharedHandler<(Arc<S>, Context, Value, CancellationToken), HandlerFuture>>;

/// A type-erased notification handler stored in the frozen [`Router`].
///
/// Like the request handler it decodes once and invokes the typed handler, but
/// it encodes nothing: notifications have no response. Malformed parameters are
/// logged and dropped without ever calling the typed handler.
pub(crate) type ErasedNotificationHandler<S> =
    Box<dyn SharedHandler<(Arc<S>, Context, Value), NotificationFuture>>;

/// A type-erased command handler stored in the frozen [`Router`].
///
/// The engine decodes `workspace/executeCommand`'s [`ExecuteCommandParams`] to
/// route by command name, then hands the raw argument array here. The erased
/// handler decodes those arguments into the typed `Args` once, invokes the
/// typed handler, and encodes its `Output` once.
///
/// [`ExecuteCommandParams`]: lsp_types::ExecuteCommandParams
pub(crate) type ErasedCommandHandler<S> =
    Box<dyn SharedHandler<(Arc<S>, Context, Vec<Value>, CancellationToken), HandlerFuture>>;

/// The synchronous, run-at-most-once initialization-dependent registration
/// callback (ADR 0017). It receives read-only `InitializeParams` and a
/// transactional [`InitializeRegistrar`]; returning `Err` discards the whole
/// transaction. Boxed `FnOnce` because the engine invokes it exactly once.
pub(crate) type ConfigureInitialize<S> = Box<dyn ConfigureInitializeCallback<S>>;

/// The future produced by the erased `on_initialize` hook: optional
/// [`ServerInfo`] to combine with the generated capabilities, or an
/// [`LspError`] that fails initialization.
type OnInitializeFuture = Pin<Box<dyn TaskFuture<Result<Option<ServerInfo>, LspError>>>>;

/// The erased `on_initialize` lifecycle hook (ADR 0018). It has the request
/// handler shape but returns optional [`ServerInfo`]; it cannot register routes
/// or replace the generated capabilities.
pub(crate) type OnInitialize<S> = Box<
    dyn SharedHandler<(Arc<S>, Context, InitializeParams, CancellationToken), OnInitializeFuture>,
>;

/// The erased `on_initialized` lifecycle hook. It has the notification handler
/// shape — the client's `initialized` notification carries no response — so it
/// resolves to `()`. The engine invokes it at most once, only after the
/// initialize transaction succeeded.
pub(crate) type OnInitialized<S> =
    Box<dyn SharedHandler<(Arc<S>, Context, InitializedParams), NotificationFuture>>;

/// The future produced by the erased `on_shutdown` hook. Success permits the
/// protocol-owned transition into shutting down; an error is returned to the
/// client and leaves the connection running.
type OnShutdownFuture = Pin<Box<dyn TaskFuture<Result<(), LspError>>>>;

/// The erased `on_shutdown` lifecycle hook (ADR 0018). The LSP request carries
/// unit parameters, and otherwise has the standard request-handler shape.
pub(crate) type OnShutdown<S> =
    Box<dyn SharedHandler<(Arc<S>, Context, (), CancellationToken), OnShutdownFuture>>;

/// The erased `on_exit` lifecycle hook. `exit` carries no parameters, so the
/// typed hook receives only the shared state and a [`Context`]; it resolves to
/// `()`, which is what keeps the engine's lifecycle-derived [`Outcome`] beyond
/// its reach.
pub(crate) type OnExit<S> = Box<dyn SharedHandler<(Arc<S>, Context), NotificationFuture>>;

/// The synchronous, panic-isolated observer for connection-level failures.
pub(crate) type ErrorHook = crate::failure::ErrorHook;

/// Wrap a typed request handler in the erased closure the [`Router`] stores.
/// Shared by [`ServerBuilder::request`] and [`ServerBuilder::feature`], which
/// differ only in whether the method also contributes a capability.
fn erase_request<S, R, H, Fut>(handler: H) -> ErasedRequestHandler<S>
where
    S: Send + Sync + 'static,
    R: Request,
    H: Fn(Arc<S>, Context, R::Params, CancellationToken) -> Fut
        + SharedHandler<(Arc<S>, Context, R::Params, CancellationToken), Fut>
        + 'static,
    Fut: Future<Output = Result<R::Result, LspError>> + TaskSend + 'static,
{
    let handler = Arc::new(handler);
    Box::new(move |state, ctx, params, ct| -> HandlerFuture {
        let handler = Arc::clone(&handler);
        Box::pin(async move {
            let parsed: R::Params =
                serde_json::from_value(params).map_err(LspError::invalid_params)?;
            let result = handler.invoke((state, ctx, parsed, ct)).await?;
            erase_value(result)
        })
    })
}

/// Wrap a typed notification handler in the erased closure the [`Router`]
/// stores. Shared by [`ServerBuilder::notification`] and
/// [`ServerBuilder::feature_notification`], which differ only in whether the
/// method also contributes a capability. Malformed parameters are logged and
/// dropped without ever calling the typed handler.
fn erase_notification<S, N, H, Fut>(handler: H) -> ErasedNotificationHandler<S>
where
    S: Send + Sync + 'static,
    N: Notification,
    H: Fn(Arc<S>, Context, N::Params) -> Fut
        + SharedHandler<(Arc<S>, Context, N::Params), Fut>
        + 'static,
    Fut: Future<Output = ()> + TaskSend + 'static,
{
    let handler = Arc::new(handler);
    Box::new(move |state, ctx, params| -> NotificationFuture {
        let handler = Arc::clone(&handler);
        Box::pin(async move {
            let parsed: N::Params = match serde_json::from_value(params) {
                Ok(parsed) => parsed,
                Err(error) => {
                    // A notification has no reply, so a decode failure is
                    // reported through tracing and dropped; later messages
                    // are unaffected (ADR 0017).
                    warn!(
                        method = N::METHOD,
                        %error,
                        "dropping notification with malformed params"
                    );
                    return;
                }
            };
            handler.invoke((state, ctx, parsed)).await;
        })
    })
}

/// The still-mutable set of handler registrations and their capability
/// contributions (ADR 0017). Both [`ServerBuilder`] and [`InitializeRegistrar`]
/// accumulate into one of these; the protocol engine [`freeze`](Self::freeze)s
/// it into a [`Router`] once the initialize transaction commits.
///
/// Each `add_*` method performs the same conflict detection the frozen table
/// relies on, returning the first [`BuildError`] to its caller, who decides
/// whether to record it (the builder) or abort the transaction (the registrar).
pub(crate) struct Registrations<S> {
    requests: HashMap<String, ErasedRequestHandler<S>>,
    notifications: HashMap<String, ErasedNotificationHandler<S>>,
    /// Post-validation hooks for protocol-owned notifications, kept apart from
    /// `notifications` so no ordinary route can ever shadow a built-in.
    built_in_hooks: HashMap<String, ErasedNotificationHandler<S>>,
    commands: HashMap<String, ErasedCommandHandler<S>>,
    capabilities: CapabilityBuilder,
    document_sync: Option<TextDocumentSyncOptions>,
}

impl<S: Send + Sync + 'static> Registrations<S> {
    fn new() -> Self {
        Self {
            requests: HashMap::new(),
            notifications: HashMap::new(),
            built_in_hooks: HashMap::new(),
            commands: HashMap::new(),
            capabilities: CapabilityBuilder::default(),
            document_sync: None,
        }
    }

    /// Register a standard feature handler and its capability contribution.
    fn add_feature<F, H, Fut>(&mut self, spec: F, handler: H) -> Result<(), BuildError>
    where
        F: FeatureSpec,
        H: Fn(Arc<S>, Context, <F::Marker as Request>::Params, CancellationToken) -> Fut
            + SharedHandler<
                (
                    Arc<S>,
                    Context,
                    <F::Marker as Request>::Params,
                    CancellationToken,
                ),
                Fut,
            > + 'static,
        Fut: Future<Output = Result<<F::Marker as Request>::Result, LspError>> + TaskSend + 'static,
    {
        let method = <F::Marker as Request>::METHOD.to_string();
        let erased = erase_request::<S, F::Marker, H, Fut>(handler);
        self.insert_request(method, erased)?;
        spec.contribute(&mut self.capabilities)
    }

    /// Register a typed custom request handler (contributes no capability).
    fn add_request<R, H, Fut>(&mut self, handler: H) -> Result<(), BuildError>
    where
        R: Request,
        H: Fn(Arc<S>, Context, R::Params, CancellationToken) -> Fut
            + SharedHandler<(Arc<S>, Context, R::Params, CancellationToken), Fut>
            + 'static,
        Fut: Future<Output = Result<R::Result, LspError>> + TaskSend + 'static,
    {
        let method = R::METHOD.to_string();
        let erased = erase_request::<S, R, H, Fut>(handler);
        self.insert_request(method, erased)
    }

    /// Insert an already-erased request handler under `method`, rejecting a
    /// reserved method or a duplicate. Shared by [`add_feature`](Self::add_feature)
    /// and [`add_request`](Self::add_request), which differ only in the capability
    /// contribution that follows a successful insert.
    fn insert_request(
        &mut self,
        method: String,
        erased: ErasedRequestHandler<S>,
    ) -> Result<(), BuildError> {
        if RESERVED_METHODS.contains(&method.as_str()) {
            return Err(BuildError::ReservedMethod(method));
        }
        if self.requests.insert(method.clone(), erased).is_some() {
            return Err(BuildError::DuplicateMethod(method));
        }
        Ok(())
    }

    /// Register a typed custom notification handler (contributes no capability).
    fn add_notification<N, H, Fut>(&mut self, handler: H) -> Result<(), BuildError>
    where
        N: Notification,
        H: Fn(Arc<S>, Context, N::Params) -> Fut
            + SharedHandler<(Arc<S>, Context, N::Params), Fut>
            + 'static,
        Fut: Future<Output = ()> + TaskSend + 'static,
    {
        let method = N::METHOD.to_string();
        let erased = erase_notification::<S, N, H, Fut>(handler);
        self.insert_notification(method, erased)
    }

    /// Register a standard notification feature handler and its capability
    /// contribution. Shares [`add_notification`](Self::add_notification)'s
    /// routing — a protocol-owned method still records a post-validation hook
    /// rather than a route.
    fn add_feature_notification<F, H, Fut>(&mut self, spec: F, handler: H) -> Result<(), BuildError>
    where
        F: NotificationFeatureSpec,
        H: Fn(Arc<S>, Context, <F::Marker as Notification>::Params) -> Fut
            + SharedHandler<(Arc<S>, Context, <F::Marker as Notification>::Params), Fut>
            + 'static,
        Fut: Future<Output = ()> + TaskSend + 'static,
    {
        let method = <F::Marker as Notification>::METHOD.to_string();
        let erased = erase_notification::<S, F::Marker, H, Fut>(handler);
        self.insert_notification(method, erased)?;
        spec.contribute(&mut self.capabilities)
    }

    /// Insert an already-erased notification handler under `method`, rejecting
    /// a reserved method or a duplicate. A protocol-owned notification records
    /// the connection's one post-validation hook; every other method becomes a
    /// Router route.
    fn insert_notification(
        &mut self,
        method: String,
        erased: ErasedNotificationHandler<S>,
    ) -> Result<(), BuildError> {
        if RESERVED_METHODS.contains(&method.as_str()) {
            return Err(BuildError::ReservedMethod(method));
        }
        let table = if ProtocolNotification::from_method(&method).is_some() {
            &mut self.built_in_hooks
        } else {
            &mut self.notifications
        };
        if table.insert(method.clone(), erased).is_some() {
            return Err(BuildError::DuplicateMethod(method));
        }
        Ok(())
    }

    /// Register a typed command beneath `workspace/executeCommand`.
    fn add_command<Args, Output, H, Fut>(
        &mut self,
        name: String,
        handler: H,
    ) -> Result<(), BuildError>
    where
        Args: DeserializeOwned + TaskSend + 'static,
        Output: Serialize + 'static,
        H: Fn(Arc<S>, Context, Args, CancellationToken) -> Fut
            + SharedHandler<(Arc<S>, Context, Args, CancellationToken), Fut>
            + 'static,
        Fut: Future<Output = Result<Output, LspError>> + TaskSend + 'static,
    {
        if name.is_empty() {
            return Err(BuildError::EmptyCommandName);
        }
        let handler = Arc::new(handler);
        let erased: ErasedCommandHandler<S> =
            Box::new(move |state, ctx, arguments, ct| -> HandlerFuture {
                let handler = Arc::clone(&handler);
                Box::pin(async move {
                    let args: Args = serde_json::from_value(Value::Array(arguments))
                        .map_err(LspError::invalid_params)?;
                    let result = handler.invoke((state, ctx, args, ct)).await?;
                    erase_value(result)
                })
            });
        if self.commands.insert(name.clone(), erased).is_some() {
            return Err(BuildError::DuplicateCommand(name));
        }
        self.capabilities.add_command(name);
        Ok(())
    }

    /// Cross-cutting validation that no single registration can detect on its
    /// own. Run both at static build time and when the initialize transaction
    /// commits, so a conditional registration cannot smuggle in a conflict.
    fn validate(&self) -> Result<(), BuildError> {
        self.capabilities.validate()?;
        self.document_sync_settings()?;
        // A command registration and an explicit `workspace/executeCommand`
        // request handler both claim the same method; they cannot coexist.
        if !self.commands.is_empty() && self.requests.contains_key(EXECUTE_COMMAND_METHOD) {
            return Err(BuildError::ExecuteCommandConflict);
        }
        Ok(())
    }

    fn document_sync_settings(&self) -> Result<DocumentSyncSettings, BuildError> {
        let save_hook = self
            .built_in_hooks
            .contains_key(ProtocolNotification::SAVE_METHOD);
        let will_save_hook = self
            .built_in_hooks
            .contains_key(ProtocolNotification::WILL_SAVE_METHOD);
        let wait_until = self.capabilities.has_will_save_wait_until();

        if let Some(explicit) = &self.document_sync {
            let save_disabled = matches!(
                explicit.save,
                Some(TextDocumentSyncSaveOptions::Supported(false))
            );
            if save_hook && save_disabled {
                return Err(BuildError::ConflictingCapability {
                    field: "textDocumentSync.save",
                });
            }
            if will_save_hook && explicit.will_save == Some(false) {
                return Err(BuildError::ConflictingCapability {
                    field: "textDocumentSync.willSave",
                });
            }
            if wait_until && explicit.will_save_wait_until == Some(false) {
                return Err(BuildError::ConflictingCapability {
                    field: "textDocumentSync.willSaveWaitUntil",
                });
            }
            if explicit.change == Some(TextDocumentSyncKind::NONE) {
                let field = if save_hook {
                    Some("textDocumentSync.save")
                } else if will_save_hook {
                    Some("textDocumentSync.willSave")
                } else if wait_until {
                    Some("textDocumentSync.willSaveWaitUntil")
                } else {
                    None
                };
                if let Some(field) = field {
                    return Err(BuildError::ConflictingCapability { field });
                }
            }
        }

        let mut options = self.document_sync.clone().unwrap_or_default();
        options.open_close.get_or_insert(true);
        options
            .change
            .get_or_insert(TextDocumentSyncKind::INCREMENTAL);
        if save_hook && options.save.is_none() {
            options.save = Some(true.into());
        }
        if will_save_hook && options.will_save.is_none() {
            options.will_save = Some(true);
        }
        if wait_until && options.will_save_wait_until.is_none() {
            options.will_save_wait_until = Some(true);
        }

        if options.change == Some(TextDocumentSyncKind::NONE) {
            options.open_close = Some(false);
            options.will_save = Some(false);
            options.will_save_wait_until = Some(false);
            options.save = Some(false.into());
            return Ok(DocumentSyncSettings {
                capability: TextDocumentSyncCapability::Kind(TextDocumentSyncKind::NONE),
                options,
            });
        }

        let capability =
            if self.document_sync.is_none() && !save_hook && !will_save_hook && !wait_until {
                TextDocumentSyncCapability::Kind(TextDocumentSyncKind::INCREMENTAL)
            } else {
                TextDocumentSyncCapability::Options(options.clone())
            };
        Ok(DocumentSyncSettings {
            capability,
            options,
        })
    }

    /// Freeze the registrations into the connection's permanent [`Router`],
    /// computing its capability catalog once from the same registrations used
    /// for dispatch (ADR 0017).
    pub(crate) fn freeze(self) -> Router<S> {
        let document_sync = self
            .document_sync_settings()
            .expect("registrations are validated before freeze");
        Router {
            requests: self.requests,
            notifications: self.notifications,
            built_in_hooks: self.built_in_hooks,
            commands: self.commands,
            capabilities: self.capabilities.finish_generated(),
            document_sync,
        }
    }
}

/// The permanently frozen table of user handlers for one connection
/// (ADR 0017). The protocol engine produces it by freezing [`Registrations`]
/// once the initialize transaction commits; no API mutates it afterward.
pub(crate) struct Router<S> {
    requests: HashMap<String, ErasedRequestHandler<S>>,
    notifications: HashMap<String, ErasedNotificationHandler<S>>,
    built_in_hooks: HashMap<String, ErasedNotificationHandler<S>>,
    commands: HashMap<String, ErasedCommandHandler<S>>,
    /// Capabilities implied by the frozen registrations, computed once at
    /// freeze time from the same registrations used for dispatch.
    capabilities: GeneratedCapabilities,
    document_sync: DocumentSyncSettings,
}

impl<S> Router<S> {
    /// The erased request handler registered for `method`, if any.
    pub(crate) fn request(&self, method: &str) -> Option<&ErasedRequestHandler<S>> {
        self.requests.get(method)
    }

    /// The erased notification handler registered for `method`, if any.
    pub(crate) fn notification(&self, method: &str) -> Option<&ErasedNotificationHandler<S>> {
        self.notifications.get(method)
    }

    /// The erased post-validation hook registered for a protocol-owned `method`,
    /// if any (ADR 0018, ADR 0023). The protocol engine has already decoded and
    /// validated by the time this hook is reached. When the built-in mutates
    /// state, the hook observes that mutation; it cannot replace the built-in.
    pub(crate) fn built_in_hook(&self, method: &str) -> Option<&ErasedNotificationHandler<S>> {
        self.built_in_hooks.get(method)
    }

    /// The erased command handler registered under `name`, if any.
    pub(crate) fn command(&self, name: &str) -> Option<&ErasedCommandHandler<S>> {
        self.commands.get(name)
    }

    /// Whether any command is registered. When true, the engine routes
    /// `workspace/executeCommand` to the command table rather than a request
    /// handler (the two are a build-time conflict and never coexist).
    pub(crate) fn has_commands(&self) -> bool {
        !self.commands.is_empty()
    }

    /// The capabilities implied by the frozen registrations. Custom requests
    /// and notifications contribute nothing; standard features and commands
    /// contribute their fields. The protocol engine layers on any
    /// protocol-owned negotiated fields separately.
    #[cfg(test)]
    pub(crate) fn capabilities(&self) -> ServerCapabilities {
        self.capabilities.standard.clone()
    }

    pub(crate) fn generated_capabilities(&self) -> GeneratedCapabilities {
        self.capabilities.clone()
    }

    pub(crate) fn document_sync(&self) -> DocumentSyncSettings {
        self.document_sync.clone()
    }
}

/// Collects static registrations for one connection before handing them to a
/// [`Server`] (ADR 0017). Registration mistakes are recorded and surfaced by
/// [`build`](Self::build); the builder methods stay chainable.
pub struct ServerBuilder<S> {
    state: Arc<S>,
    file_provider: SharedFileProvider,
    registrations: Registrations<S>,
    configure_initialize: Option<ConfigureInitialize<S>>,
    on_initialize: Option<OnInitialize<S>>,
    on_initialized: Option<OnInitialized<S>>,
    on_shutdown: Option<OnShutdown<S>>,
    on_exit: Option<OnExit<S>>,
    error_hook: Option<ErrorHook>,
    layers: Vec<UserLayer<S>>,
    resource_policy: crate::ResourcePolicy,
    /// First registration error seen, if any. Reported by `build`.
    error: Option<BuildError>,
}

impl<S: Send + Sync + 'static> ServerBuilder<S> {
    fn new(state: S) -> Self {
        Self {
            state: Arc::new(state),
            file_provider: crate::file_provider::default_file_provider(),
            registrations: Registrations::new(),
            configure_initialize: None,
            on_initialize: None,
            on_initialized: None,
            on_shutdown: None,
            on_exit: None,
            error_hook: None,
            layers: Vec::new(),
            resource_policy: crate::ResourcePolicy::default(),
            error: None,
        }
    }

    /// Configure the connection's protocol-owned text-document synchronization.
    /// Unspecified open/close and change fields retain the framework defaults;
    /// save-related fields are inferred from typed registrations.
    pub fn text_document_sync(mut self, options: TextDocumentSyncOptions) -> Self {
        self.registrations.document_sync = Some(options);
        self
    }

    /// Replace the provider used to resolve resources that are not open in
    /// the editor. The provider is owned by this connection's workspace.
    pub fn file_provider<P: FileProvider>(mut self, provider: P) -> Self {
        self.file_provider = erase(provider);
        self
    }

    /// Register a standard LSP feature and its capability contribution.
    ///
    /// `spec` is a descriptor from [`lspf::features`](crate::features) — for
    /// example [`features::hover()`](crate::features::hover) or
    /// [`features::completion(options)`](crate::features::completion). It fixes
    /// the wire method, the typed parameter and result, and the single
    /// capability field the feature advertises. The handler has the same shape
    /// as a custom [`request`](Self::request) handler for that method.
    ///
    /// Registering two handlers for the same method is a
    /// [`BuildError::DuplicateMethod`]; two features that disagree on a
    /// singular capability field are a
    /// [`BuildError::ConflictingCapability`]. Both are reported by
    /// [`build`](Self::build).
    pub fn feature<F, H, Fut>(mut self, spec: F, handler: H) -> Self
    where
        F: FeatureSpec,
        H: Fn(Arc<S>, Context, <F::Marker as Request>::Params, CancellationToken) -> Fut
            + SharedHandler<
                (
                    Arc<S>,
                    Context,
                    <F::Marker as Request>::Params,
                    CancellationToken,
                ),
                Fut,
            > + 'static,
        Fut: Future<Output = Result<<F::Marker as Request>::Result, LspError>> + TaskSend + 'static,
    {
        if let Err(err) = self.registrations.add_feature(spec, handler) {
            self.record(err);
        }
        self
    }

    /// Register a typed custom request handler.
    ///
    /// The marker `R` implements [`lspf::types::request::Request`](crate::types)
    /// (lspf's re-export of `lsp_types::request::Request`) and thereby fixes
    /// the wire method, parameter type, and result type used by dispatch. The
    /// handler receives the shared application state, a [`Context`], the
    /// decoded parameters, and a request-scoped [`CancellationToken`].
    ///
    /// Custom requests add nothing to `ServerCapabilities`. Registering two
    /// handlers for the same method, or a method the framework reserves, is a
    /// [`BuildError`] reported by [`build`](Self::build).
    pub fn request<R, H, Fut>(mut self, handler: H) -> Self
    where
        R: Request,
        H: Fn(Arc<S>, Context, R::Params, CancellationToken) -> Fut
            + SharedHandler<(Arc<S>, Context, R::Params, CancellationToken), Fut>
            + 'static,
        Fut: Future<Output = Result<R::Result, LspError>> + TaskSend + 'static,
    {
        if let Err(err) = self.registrations.add_request::<R, H, Fut>(handler) {
            self.record(err);
        }
        self
    }

    /// Register a typed custom notification handler.
    ///
    /// The marker `N` implements
    /// [`lspf::types::notification::Notification`](crate::types) (lspf's
    /// re-export of `lsp_types::notification::Notification`) and fixes the wire
    /// method and parameter type. The handler receives the shared application
    /// state, a [`Context`], and the decoded parameters. A notification has no
    /// response, so the handler returns `()` and there is no cancellation token.
    ///
    /// Custom notifications add nothing to `ServerCapabilities`. Malformed
    /// parameters are logged and dropped without invoking the handler.
    /// Registering two handlers for the same method, or a method the framework
    /// reserves, is a [`BuildError`] reported by [`build`](Self::build).
    pub fn notification<N, H, Fut>(mut self, handler: H) -> Self
    where
        N: Notification,
        H: Fn(Arc<S>, Context, N::Params) -> Fut
            + SharedHandler<(Arc<S>, Context, N::Params), Fut>
            + 'static,
        Fut: Future<Output = ()> + TaskSend + 'static,
    {
        if let Err(err) = self.registrations.add_notification::<N, H, Fut>(handler) {
            self.record(err);
        }
        self
    }

    /// Register a standard LSP notification feature and its capability
    /// contribution.
    ///
    /// `spec` is a descriptor from [`lspf::features`](crate::features) — for
    /// example [`features::did_create_files(options)`](crate::features::did_create_files).
    /// It fixes the wire method, the typed parameter, and the capability field
    /// the feature advertises. The handler has the same shape as a custom
    /// [`notification`](Self::notification) handler for that method.
    ///
    /// Registering two handlers for the same method is a
    /// [`BuildError::DuplicateMethod`]; two features that disagree on a
    /// singular capability field are a
    /// [`BuildError::ConflictingCapability`]. Both are reported by
    /// [`build`](Self::build).
    pub fn feature_notification<F, H, Fut>(mut self, spec: F, handler: H) -> Self
    where
        F: NotificationFeatureSpec,
        H: Fn(Arc<S>, Context, <F::Marker as Notification>::Params) -> Fut
            + SharedHandler<(Arc<S>, Context, <F::Marker as Notification>::Params), Fut>
            + 'static,
        Fut: Future<Output = ()> + TaskSend + 'static,
    {
        if let Err(err) = self.registrations.add_feature_notification(spec, handler) {
            self.record(err);
        }
        self
    }

    /// Register a typed command dispatched on `workspace/executeCommand`.
    ///
    /// The command is invoked when the editor sends `workspace/executeCommand`
    /// with a matching `name`; its complete `arguments` array is decoded into
    /// `Args` (tuple, struct, and `Vec` types alike), and an absent `arguments`
    /// field decodes as an empty array. The handler's `Output` is returned as
    /// the command result. The
    /// handler receives the shared application state, a [`Context`], the typed
    /// arguments, and a request-scoped [`CancellationToken`]. `Args` and
    /// `Output` are bounded by the serialization required to cross the wire.
    ///
    /// Each registered `name` merges into one de-duplicated execute-command
    /// capability that preserves registration order (ADR 0022). An
    /// empty name, two handlers for the same name, or a command alongside an
    /// explicit `workspace/executeCommand` [`request`](Self::request) handler
    /// is a [`BuildError`] reported by [`build`](Self::build).
    pub fn command<Args, Output, H, Fut>(mut self, name: impl Into<String>, handler: H) -> Self
    where
        Args: DeserializeOwned + TaskSend + 'static,
        Output: Serialize + 'static,
        H: Fn(Arc<S>, Context, Args, CancellationToken) -> Fut
            + SharedHandler<(Arc<S>, Context, Args, CancellationToken), Fut>
            + 'static,
        Fut: Future<Output = Result<Output, LspError>> + TaskSend + 'static,
    {
        if let Err(err) = self
            .registrations
            .add_command::<Args, Output, H, Fut>(name.into(), handler)
        {
            self.record(err);
        }
        self
    }

    /// Register the sole synchronous initialization-dependent registration
    /// callback (ADR 0017).
    ///
    /// After a valid `initialize` request the engine runs `callback` exactly
    /// once against a transactional [`InitializeRegistrar`], passing read-only
    /// `InitializeParams`. The callback may conditionally register features,
    /// requests, notifications, and commands; returning `Err` discards the
    /// whole transaction. It performs no I/O and cannot `.await`.
    ///
    /// Supplying `configure_initialize` more than once is a
    /// [`BuildError::DuplicateConfigureInitialize`] reported by
    /// [`build`](Self::build).
    pub fn configure_initialize<F>(mut self, callback: F) -> Self
    where
        F: FnOnce(&InitializeParams, &mut InitializeRegistrar<S>) -> Result<(), LspError>
            + TaskSend
            + 'static,
    {
        if self.configure_initialize.is_some() {
            self.record(BuildError::DuplicateConfigureInitialize);
        } else {
            self.configure_initialize = Some(Box::new(callback));
        }
        self
    }

    /// Register the `on_initialize` lifecycle hook (ADR 0018).
    ///
    /// The hook runs after the `Workspace`, `Documents`, and negotiated
    /// position encoding are established and after the Router is frozen, but
    /// before the `InitializeResult` is sent. It may contribute an optional
    /// [`ServerInfo`], but it cannot register routes or replace the generated
    /// `ServerCapabilities`. Returning `Err` fails initialization.
    ///
    /// Supplying `on_initialize` more than once is a
    /// [`BuildError::DuplicateLifecycleHook`] reported by [`build`](Self::build).
    pub fn on_initialize<H, Fut>(mut self, hook: H) -> Self
    where
        H: Fn(Arc<S>, Context, InitializeParams, CancellationToken) -> Fut
            + SharedHandler<(Arc<S>, Context, InitializeParams, CancellationToken), Fut>
            + 'static,
        Fut: Future<Output = Result<Option<ServerInfo>, LspError>> + TaskSend + 'static,
    {
        if self.on_initialize.is_some() {
            self.record(BuildError::DuplicateLifecycleHook("on_initialize"));
        } else {
            // The hook runs once, so — unlike the many-shot request handlers —
            // it needs no `Arc`; the erasing closure just boxes its future.
            self.on_initialize = Some(Box::new(
                move |state, ctx, params, ct| -> OnInitializeFuture {
                    Box::pin(hook.invoke((state, ctx, params, ct)))
                },
            ));
        }
        self
    }

    /// Register the `on_initialized` lifecycle hook (ADR 0024).
    ///
    /// The hook runs at most once, and only after a successful initialize
    /// transaction: when the client's `initialized` notification arrives while
    /// the connection is running. It receives the shared application state, a
    /// [`Context`], and the typed [`InitializedParams`]. A notification has no
    /// response, so the hook resolves to `()`. An `initialized` notification
    /// received before `initialize` or after `shutdown` is ignored without
    /// consuming the hook, and malformed parameters are dropped.
    ///
    /// Supplying `on_initialized` more than once is a
    /// [`BuildError::DuplicateLifecycleHook`] reported by [`build`](Self::build).
    pub fn on_initialized<H, Fut>(mut self, hook: H) -> Self
    where
        H: Fn(Arc<S>, Context, InitializedParams) -> Fut
            + SharedHandler<(Arc<S>, Context, InitializedParams), Fut>
            + 'static,
        Fut: Future<Output = ()> + TaskSend + 'static,
    {
        if self.on_initialized.is_some() {
            self.record(BuildError::DuplicateLifecycleHook("on_initialized"));
        } else {
            self.on_initialized = Some(Box::new(move |state, ctx, params| -> NotificationFuture {
                Box::pin(hook.invoke((state, ctx, params)))
            }));
        }
        self
    }

    /// Register the `on_shutdown` lifecycle hook (ADR 0018).
    ///
    /// The hook runs after a successful initialize transaction and before the
    /// protocol engine enters its shutting-down state. It receives the shared
    /// application state, a live [`Context`], the shutdown request's unit
    /// parameters, and its [`CancellationToken`]. Returning `Ok(())` permits
    /// shutdown; returning [`LspError`] sends that error response and leaves
    /// the connection running so the client may retry or continue using it.
    ///
    /// Supplying `on_shutdown` more than once is a
    /// [`BuildError::DuplicateLifecycleHook`] reported by [`build`](Self::build).
    pub fn on_shutdown<H, Fut>(mut self, hook: H) -> Self
    where
        H: Fn(Arc<S>, Context, (), CancellationToken) -> Fut
            + SharedHandler<(Arc<S>, Context, (), CancellationToken), Fut>
            + 'static,
        Fut: Future<Output = Result<(), LspError>> + TaskSend + 'static,
    {
        if self.on_shutdown.is_some() {
            self.record(BuildError::DuplicateLifecycleHook("on_shutdown"));
        } else {
            self.on_shutdown = Some(Box::new(
                move |state, ctx, params, ct| -> OnShutdownFuture {
                    Box::pin(hook.invoke((state, ctx, params, ct)))
                },
            ));
        }
        self
    }

    /// Register the `on_exit` lifecycle hook (ADR 0018, ADR 0024).
    ///
    /// The hook runs when the peer's `exit` notification arrives after a
    /// successful initialize transaction, before the protocol engine computes
    /// the exit outcome. It receives the shared application state and a
    /// [`Context`] — the notification-handler shape; `exit` carries no
    /// parameters — and resolves to `()`, so it cannot override the
    /// lifecycle-derived outcome: the reported LSP exit code is still 0 after
    /// a successful `shutdown` and 1 otherwise. An `exit` received before
    /// `initialize` closes the connection with code 1 without running the
    /// hook — no [`Workspace`](crate::Workspace) exists to hand it.
    ///
    /// Supplying `on_exit` more than once is a
    /// [`BuildError::DuplicateLifecycleHook`] reported by [`build`](Self::build).
    pub fn on_exit<H, Fut>(mut self, hook: H) -> Self
    where
        H: Fn(Arc<S>, Context) -> Fut + SharedHandler<(Arc<S>, Context), Fut> + 'static,
        Fut: Future<Output = ()> + TaskSend + 'static,
    {
        if self.on_exit.is_some() {
            self.record(BuildError::DuplicateLifecycleHook("on_exit"));
        } else {
            self.on_exit = Some(Box::new(move |state, ctx| -> NotificationFuture {
                Box::pin(hook.invoke((state, ctx)))
            }));
        }
        self
    }

    /// Register the connection-level error hook.
    ///
    /// The hook receives stable failure categories and non-sensitive identity
    /// only. It runs outside the user Layer chain, and any panic from the hook
    /// is isolated so it cannot change protocol responses or connection
    /// cleanup. The hook must be `Send + Sync` on every target because its
    /// reporter is shared by connection handles. Registering it more than once
    /// is reported by [`build`](Self::build).
    pub fn on_error<H>(mut self, hook: H) -> Self
    where
        H: Fn(crate::ConnectionFailure)
            + SharedHandler<(crate::ConnectionFailure,), ()>
            + Send
            + Sync
            + 'static,
    {
        if self.error_hook.is_some() {
            self.record(BuildError::DuplicateErrorHook);
        } else {
            self.error_hook = Some(Arc::new(hook));
        }
        self
    }

    /// Register a user Layer around normalized user dispatch.
    ///
    /// The last registered Layer is outermost among user Layers. Framework
    /// panic isolation, tracing, and concurrency limiting remain outside it.
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: Layer<S>,
    {
        self.layers.push(Arc::new(layer));
        self
    }

    /// Set the inbound-request budget in the connection's resource policy.
    ///
    /// This compatibility shorthand updates
    /// [`ResourcePolicy::max_inbound_requests`](crate::ResourcePolicy::max_inbound_requests).
    /// Zero is rejected by [`build`](Self::build).
    pub fn concurrency_limit(mut self, limit: usize) -> Self {
        if limit == 0 {
            self.record(BuildError::InvalidConcurrencyLimit);
        } else {
            self.resource_policy.max_inbound_requests = limit;
        }
        self
    }

    /// Set the outbound-message budget in the connection's resource policy.
    ///
    /// This compatibility shorthand updates
    /// [`ResourcePolicy::max_outbound_messages`](crate::ResourcePolicy::max_outbound_messages).
    /// The historical method name is retained for source compatibility; the
    /// value is now a hard queue budget. Zero is rejected by [`build`](Self::build).
    pub fn outbound_warning_threshold(mut self, threshold: usize) -> Self {
        if threshold == 0 {
            self.record(BuildError::InvalidOutboundWarningThreshold);
        } else {
            self.resource_policy.max_outbound_messages = threshold;
        }
        self
    }

    /// Replace all finite budgets and deadlines owned by this connection.
    ///
    /// Invalid zero budgets or enabled zero deadlines are reported by
    /// [`build`](Self::build). An outbound-request deadline may be explicitly
    /// disabled by setting
    /// [`ResourcePolicy::outbound_request_timeout`](crate::ResourcePolicy::outbound_request_timeout)
    /// to `None`.
    pub fn resource_policy(mut self, policy: crate::ResourcePolicy) -> Self {
        self.resource_policy = policy;
        self
    }

    /// Validate the complete static registration set and return the [`Server`].
    ///
    /// Performs no I/O and does not run `configure_initialize`; the Router is
    /// frozen later, when the engine commits the initialize transaction. Returns
    /// the first [`BuildError`] recorded during registration, if any.
    pub fn build(mut self) -> Result<Server<S>, BuildError> {
        if let Err(err) = self.resource_policy.validate() {
            self.record(err);
        }
        if let Err(err) = self.registrations.validate() {
            self.record(err);
        }
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(Server {
            state: self.state,
            file_provider: self.file_provider,
            registrations: self.registrations,
            configure_initialize: self.configure_initialize,
            on_initialize: self.on_initialize,
            on_initialized: self.on_initialized,
            on_shutdown: self.on_shutdown,
            on_exit: self.on_exit,
            error_hook: self.error_hook,
            layers: self.layers,
            resource_policy: self.resource_policy,
        })
    }

    /// Record the first registration error; later ones are dropped because the
    /// first already fails `build`.
    fn record(&mut self, error: BuildError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }
}

/// The transactional registrar handed to `configure_initialize` (ADR 0017).
///
/// It offers the same `feature`, `request`, `notification`, and `command`
/// registration semantics as the static [`ServerBuilder`], starting from a
/// view of all static registrations, but exposes no `layer`, nested
/// `configure_initialize`, `build`, or dynamic-client operation. Conditional
/// registration mistakes are recorded and abort the whole transaction when the
/// engine commits it, so no partial route or capability ever becomes visible.
pub struct InitializeRegistrar<S> {
    registrations: Registrations<S>,
    /// First conditional registration error seen, if any. Once set, later
    /// registrations are skipped and the transaction fails on commit.
    error: Option<BuildError>,
}

impl<S: Send + Sync + 'static> InitializeRegistrar<S> {
    pub(crate) fn new(registrations: Registrations<S>) -> Self {
        Self {
            registrations,
            error: None,
        }
    }

    /// Conditionally register a standard feature and its capability. See
    /// [`ServerBuilder::feature`].
    pub fn feature<F, H, Fut>(&mut self, spec: F, handler: H) -> &mut Self
    where
        F: FeatureSpec,
        H: Fn(Arc<S>, Context, <F::Marker as Request>::Params, CancellationToken) -> Fut
            + SharedHandler<
                (
                    Arc<S>,
                    Context,
                    <F::Marker as Request>::Params,
                    CancellationToken,
                ),
                Fut,
            > + 'static,
        Fut: Future<Output = Result<<F::Marker as Request>::Result, LspError>> + TaskSend + 'static,
    {
        self.try_register(|r| r.add_feature(spec, handler))
    }

    /// Conditionally register a typed custom request. See
    /// [`ServerBuilder::request`].
    pub fn request<R, H, Fut>(&mut self, handler: H) -> &mut Self
    where
        R: Request,
        H: Fn(Arc<S>, Context, R::Params, CancellationToken) -> Fut
            + SharedHandler<(Arc<S>, Context, R::Params, CancellationToken), Fut>
            + 'static,
        Fut: Future<Output = Result<R::Result, LspError>> + TaskSend + 'static,
    {
        self.try_register(|r| r.add_request::<R, H, Fut>(handler))
    }

    /// Conditionally register a typed custom notification. See
    /// [`ServerBuilder::notification`].
    pub fn notification<N, H, Fut>(&mut self, handler: H) -> &mut Self
    where
        N: Notification,
        H: Fn(Arc<S>, Context, N::Params) -> Fut
            + SharedHandler<(Arc<S>, Context, N::Params), Fut>
            + 'static,
        Fut: Future<Output = ()> + TaskSend + 'static,
    {
        self.try_register(|r| r.add_notification::<N, H, Fut>(handler))
    }

    /// Conditionally register a standard notification feature and its
    /// capability. See [`ServerBuilder::feature_notification`].
    pub fn feature_notification<F, H, Fut>(&mut self, spec: F, handler: H) -> &mut Self
    where
        F: NotificationFeatureSpec,
        H: Fn(Arc<S>, Context, <F::Marker as Notification>::Params) -> Fut
            + SharedHandler<(Arc<S>, Context, <F::Marker as Notification>::Params), Fut>
            + 'static,
        Fut: Future<Output = ()> + TaskSend + 'static,
    {
        self.try_register(|r| r.add_feature_notification(spec, handler))
    }

    /// Conditionally register a typed command. See [`ServerBuilder::command`].
    pub fn command<Args, Output, H, Fut>(
        &mut self,
        name: impl Into<String>,
        handler: H,
    ) -> &mut Self
    where
        Args: DeserializeOwned + TaskSend + 'static,
        Output: Serialize + 'static,
        H: Fn(Arc<S>, Context, Args, CancellationToken) -> Fut
            + SharedHandler<(Arc<S>, Context, Args, CancellationToken), Fut>
            + 'static,
        Fut: Future<Output = Result<Output, LspError>> + TaskSend + 'static,
    {
        self.try_register(|r| r.add_command::<Args, Output, H, Fut>(name.into(), handler))
    }

    /// Apply one registration, recording its first error and skipping later
    /// ones so a broken transaction cannot accumulate more partial state.
    fn try_register(
        &mut self,
        op: impl FnOnce(&mut Registrations<S>) -> Result<(), BuildError>,
    ) -> &mut Self {
        if self.error.is_none()
            && let Err(err) = op(&mut self.registrations)
        {
            self.error = Some(err);
        }
        self
    }

    /// Commit the transaction: on success return the extended, validated
    /// registrations to be frozen; otherwise the first recorded error. Called
    /// by the engine only after `configure_initialize` returns `Ok`.
    pub(crate) fn commit(self) -> Result<Registrations<S>, BuildError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        self.registrations.validate()?;
        Ok(self.registrations)
    }
}

/// Owns exactly one LSP connection: its application state, the static
/// registrations awaiting the initialize transaction, the optional
/// initialization-dependent callback, and the lifecycle hooks (`on_initialize`,
/// `on_initialized`, `on_shutdown`, `on_exit`) (ADR 0017, ADR 0018). A second connection
/// requires a second `Server`; connection state is never shared between
/// servers.
pub struct Server<S> {
    pub(crate) state: Arc<S>,
    pub(crate) file_provider: SharedFileProvider,
    pub(crate) registrations: Registrations<S>,
    pub(crate) configure_initialize: Option<ConfigureInitialize<S>>,
    pub(crate) on_initialize: Option<OnInitialize<S>>,
    pub(crate) on_initialized: Option<OnInitialized<S>>,
    pub(crate) on_shutdown: Option<OnShutdown<S>>,
    pub(crate) on_exit: Option<OnExit<S>>,
    pub(crate) error_hook: Option<ErrorHook>,
    pub(crate) layers: Vec<UserLayer<S>>,
    pub(crate) resource_policy: crate::ResourcePolicy,
}

impl<S: Send + Sync + 'static> Server<S> {
    /// Begin building a connection-owned server around one application state
    /// value. Every handler for the connection shares `state` as `Arc<S>`.
    pub fn builder(state: S) -> ServerBuilder<S> {
        ServerBuilder::new(state)
    }

    /// Freeze the static registrations into a [`Router`] for inspection,
    /// bypassing the initialize transaction. Test-only: at runtime the engine
    /// freezes the Router after `configure_initialize` commits.
    #[cfg(test)]
    pub(crate) fn into_router(self) -> Router<S> {
        self.registrations.freeze()
    }

    /// Drive this server to completion over a custom [`Transport`](crate::Transport).
    ///
    /// Returns when the peer sends `exit`, the transport closes, the writer
    /// fails, or a failed initialize transaction terminates the connection —
    /// every one of which runs the engine's single close operation first. The
    /// returned [`Outcome`](crate::Outcome) names that ending and carries the
    /// LSP exit code; serving never terminates the process, so the caller
    /// decides what the outcome means. A reader transport error is returned as
    /// [`Error::Transport`](crate::Error::Transport) instead, and on native
    /// targets serving without a running Tokio runtime returns the
    /// [`Error`](crate::Error) variant `RuntimeRequired`.
    #[cfg(any(feature = "runtime-tokio", target_arch = "wasm32"))]
    pub async fn serve<T>(self, transport: T) -> crate::Result<crate::Outcome>
    where
        T: crate::Transport,
    {
        crate::engine::run(self, transport).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::request::{ExecuteCommand, HoverRequest, Shutdown};
    use lsp_types::{CompletionOptions, HoverProviderCapability};

    /// A marker for a custom method, reusing an lsp-types request type only to
    /// exercise registration without inventing wire types in the test.
    struct DummyState;

    async fn ok_hover(
        _state: Arc<DummyState>,
        _ctx: Context,
        _params: lsp_types::HoverParams,
        _ct: CancellationToken,
    ) -> Result<Option<lsp_types::Hover>, LspError> {
        Ok(None)
    }

    async fn ok_completion(
        _state: Arc<DummyState>,
        _ctx: Context,
        _params: lsp_types::CompletionParams,
        _ct: CancellationToken,
    ) -> Result<Option<lsp_types::CompletionResponse>, LspError> {
        Ok(None)
    }

    async fn ok_resolve(
        _state: Arc<DummyState>,
        _ctx: Context,
        item: lsp_types::CompletionItem,
        _ct: CancellationToken,
    ) -> Result<lsp_types::CompletionItem, LspError> {
        Ok(item)
    }

    async fn noop_command(
        _state: Arc<DummyState>,
        _ctx: Context,
        _args: Vec<String>,
        _ct: CancellationToken,
    ) -> Result<(), LspError> {
        Ok(())
    }

    async fn noop_notification(_state: Arc<DummyState>, _ctx: Context, _params: ()) {}

    #[test]
    fn duplicate_request_method_is_a_build_error() {
        let err = Server::builder(DummyState)
            .request::<HoverRequest, _, _>(ok_hover)
            .request::<HoverRequest, _, _>(ok_hover)
            .build()
            .err()
            .expect("second registration for the same method must fail");
        assert_eq!(
            err,
            BuildError::DuplicateMethod("textDocument/hover".to_string())
        );
    }

    #[test]
    fn registering_a_reserved_method_is_a_build_error() {
        async fn shutdown_handler(
            _state: Arc<DummyState>,
            _ctx: Context,
            _params: (),
            _ct: CancellationToken,
        ) -> Result<(), LspError> {
            Ok(())
        }
        let err = Server::builder(DummyState)
            .request::<Shutdown, _, _>(shutdown_handler)
            .build()
            .err()
            .expect("shutdown is framework-reserved");
        assert_eq!(err, BuildError::ReservedMethod("shutdown".to_string()));
    }

    #[test]
    fn a_single_registration_builds_and_advertises_no_extra_capabilities() {
        let server = Server::builder(DummyState)
            .request::<HoverRequest, _, _>(ok_hover)
            .build()
            .expect("a lone custom request builds");
        let router = server.into_router();
        assert!(router.request("textDocument/hover").is_some());
        assert!(router.request("nope").is_none());
        assert_eq!(
            router.capabilities(),
            ServerCapabilities::default(),
            "custom requests must not contribute capabilities"
        );
    }

    #[test]
    fn a_reserved_notification_method_is_a_build_error() {
        let err = Server::builder(DummyState)
            .notification::<lsp_types::notification::Exit, _, _>(noop_notification)
            .build()
            .err()
            .expect("exit is framework-reserved");
        assert_eq!(err, BuildError::ReservedMethod("exit".to_string()));
    }

    #[test]
    fn a_duplicate_notification_method_is_a_build_error() {
        let err = Server::builder(DummyState)
            .notification::<lsp_types::notification::DidChangeConfiguration, _, _>(
                |_s, _c, _p: lsp_types::DidChangeConfigurationParams| async {},
            )
            .notification::<lsp_types::notification::DidChangeConfiguration, _, _>(
                |_s, _c, _p: lsp_types::DidChangeConfigurationParams| async {},
            )
            .build()
            .err()
            .expect("a repeated notification method must fail");
        assert_eq!(
            err,
            BuildError::DuplicateMethod("workspace/didChangeConfiguration".to_string())
        );
    }

    #[test]
    fn workspace_mutation_hooks_contribute_no_catalog_capabilities() {
        let server = Server::builder(DummyState)
            .notification::<lsp_types::notification::DidChangeConfiguration, _, _>(
                |_s, _c, _p: lsp_types::DidChangeConfigurationParams| async {},
            )
            .build()
            .expect("a lone notification builds");
        let router = server.into_router();
        assert!(
            router
                .built_in_hook("workspace/didChangeConfiguration")
                .is_some()
        );
        assert!(
            router
                .notification("workspace/didChangeConfiguration")
                .is_none()
        );
        assert_eq!(router.capabilities(), ServerCapabilities::default());
    }

    #[test]
    fn a_document_sync_registration_records_a_hook_not_a_route() {
        let server = Server::builder(DummyState)
            .notification::<lsp_types::notification::DidOpenTextDocument, _, _>(
                |_s, _c, _p: lsp_types::DidOpenTextDocumentParams| async {},
            )
            .notification::<lsp_types::notification::DidSaveTextDocument, _, _>(
                |_s, _c, _p: lsp_types::DidSaveTextDocumentParams| async {},
            )
            .build()
            .expect("one hook and one ordinary notification build");
        let router = server.into_router();

        assert!(
            router.built_in_hook("textDocument/didOpen").is_some(),
            "a built-in document notification records a post-validation hook"
        );
        assert!(
            router.notification("textDocument/didOpen").is_none(),
            "the hook is not a Router route, so it cannot shadow the built-in"
        );
        assert!(router.notification("textDocument/didSave").is_none());
        assert!(
            router.built_in_hook("textDocument/didSave").is_some(),
            "didSave is protocol-validated before its typed hook runs"
        );
    }

    #[test]
    fn a_progress_cancel_registration_records_a_hook_not_a_route() {
        let server = Server::builder(DummyState)
            .notification::<lsp_types::notification::WorkDoneProgressCancel, _, _>(
                |_s, _c, _p: lsp_types::WorkDoneProgressCancelParams| async {},
            )
            .build()
            .expect("a lone progress-cancel hook builds");
        let router = server.into_router();

        assert!(
            router
                .built_in_hook("window/workDoneProgress/cancel")
                .is_some(),
            "the progress-cancel built-in records a post-validation hook"
        );
        assert!(
            router
                .notification("window/workDoneProgress/cancel")
                .is_none(),
            "the hook is not a Router route, so it cannot replace the built-in"
        );
        assert_eq!(
            router.capabilities(),
            ServerCapabilities::default(),
            "a progress-cancel hook contributes no capabilities"
        );
    }

    #[test]
    fn a_duplicate_document_hook_is_a_build_error() {
        let err = Server::builder(DummyState)
            .notification::<lsp_types::notification::DidChangeTextDocument, _, _>(
                |_s, _c, _p: lsp_types::DidChangeTextDocumentParams| async {},
            )
            .notification::<lsp_types::notification::DidChangeTextDocument, _, _>(
                |_s, _c, _p: lsp_types::DidChangeTextDocumentParams| async {},
            )
            .build()
            .err()
            .expect("a built-in notification takes at most one hook");
        assert_eq!(
            err,
            BuildError::DuplicateMethod("textDocument/didChange".to_string())
        );
    }

    #[test]
    fn document_hooks_contribute_no_capabilities() {
        let without_hook = Server::builder(DummyState)
            .build()
            .expect("an empty server builds")
            .into_router()
            .capabilities();
        let with_hook = Server::builder(DummyState)
            .notification::<lsp_types::notification::DidCloseTextDocument, _, _>(
                |_s, _c, _p: lsp_types::DidCloseTextDocumentParams| async {},
            )
            .build()
            .expect("a lone document hook builds")
            .into_router()
            .capabilities();
        // Compared against a hookless build rather than asserting an absolute
        // set: what a built-in itself advertises is the built-in's business,
        // and observing one must not change it either way.
        assert_eq!(
            with_hook, without_hook,
            "observing a built-in advertises nothing the built-in did not"
        );
    }

    #[test]
    fn an_empty_command_name_is_a_build_error() {
        let err = Server::builder(DummyState)
            .command::<Vec<String>, (), _, _>("", noop_command)
            .build()
            .err()
            .expect("an empty command name must fail");
        assert_eq!(err, BuildError::EmptyCommandName);
    }

    #[test]
    fn a_duplicate_command_name_is_a_build_error() {
        let err = Server::builder(DummyState)
            .command::<Vec<String>, (), _, _>("my.cmd", noop_command)
            .command::<Vec<String>, (), _, _>("my.cmd", noop_command)
            .build()
            .err()
            .expect("a repeated command name must fail");
        assert_eq!(err, BuildError::DuplicateCommand("my.cmd".to_string()));
    }

    #[test]
    fn commands_alongside_an_explicit_execute_command_handler_conflict() {
        async fn raw_execute(
            _state: Arc<DummyState>,
            _ctx: Context,
            _params: lsp_types::ExecuteCommandParams,
            _ct: CancellationToken,
        ) -> Result<Option<serde_json::Value>, LspError> {
            Ok(None)
        }
        let err = Server::builder(DummyState)
            .command::<Vec<String>, (), _, _>("my.cmd", noop_command)
            .request::<ExecuteCommand, _, _>(raw_execute)
            .build()
            .err()
            .expect("a command and a raw execute-command handler cannot coexist");
        assert_eq!(err, BuildError::ExecuteCommandConflict);
    }

    #[test]
    fn registered_commands_contribute_one_execute_command_capability() {
        let server = Server::builder(DummyState)
            .command::<Vec<String>, (), _, _>("b.cmd", noop_command)
            .command::<Vec<String>, (), _, _>("a.cmd", noop_command)
            .build()
            .expect("commands build");
        let provider = server
            .into_router()
            .capabilities()
            .execute_command_provider
            .expect("commands advertise an execute-command capability");
        assert_eq!(
            provider.commands,
            vec!["b.cmd".to_string(), "a.cmd".to_string()],
            "command names merge into one de-duplicated, registration-order list"
        );
    }

    #[test]
    fn hover_feature_sets_only_hover_provider() {
        let server = Server::builder(DummyState)
            .feature(crate::features::hover(), ok_hover)
            .build()
            .expect("hover builds");
        let router = server.into_router();
        let caps = router.capabilities();
        assert_eq!(
            caps.hover_provider,
            Some(HoverProviderCapability::Simple(true))
        );
        assert_eq!(caps.completion_provider, None);
        assert!(router.request("textDocument/hover").is_some());
    }

    #[test]
    fn hover_and_completion_merge_independent_of_order() {
        let options = CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..CompletionOptions::default()
        };
        let hover_first = Server::builder(DummyState)
            .feature(crate::features::hover(), ok_hover)
            .feature(crate::features::completion(options.clone()), ok_completion)
            .build()
            .expect("hover then completion builds")
            .into_router()
            .capabilities();
        let completion_first = Server::builder(DummyState)
            .feature(crate::features::completion(options.clone()), ok_completion)
            .feature(crate::features::hover(), ok_hover)
            .build()
            .expect("completion then hover builds")
            .into_router()
            .capabilities();
        assert_eq!(
            hover_first, completion_first,
            "capability merge is independent of registration order"
        );
        assert_eq!(
            hover_first.completion_provider,
            Some(options),
            "completion advertises the supplied options"
        );
    }

    #[test]
    fn a_duplicate_feature_is_a_build_error_not_last_write_wins() {
        let err = Server::builder(DummyState)
            .feature(crate::features::hover(), ok_hover)
            .feature(crate::features::hover(), ok_hover)
            .build()
            .err()
            .expect("registering hover twice must fail");
        assert_eq!(
            err,
            BuildError::DuplicateMethod("textDocument/hover".to_string())
        );
    }

    #[test]
    fn completion_and_resolve_merge_into_one_capability_independent_of_order() {
        let options = || CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..CompletionOptions::default()
        };
        let base_first = Server::builder(DummyState)
            .feature(crate::features::completion(options()), ok_completion)
            .feature(crate::features::completion_resolve(), ok_resolve)
            .build()
            .expect("completion then resolve builds")
            .into_router();
        let resolve_first = Server::builder(DummyState)
            .feature(crate::features::completion_resolve(), ok_resolve)
            .feature(crate::features::completion(options()), ok_completion)
            .build()
            .expect("resolve then completion builds")
            .into_router();

        assert_eq!(
            base_first.capabilities(),
            resolve_first.capabilities(),
            "the family merge is independent of registration order"
        );
        let merged = base_first
            .capabilities()
            .completion_provider
            .expect("the family emits one completionProvider capability");
        assert_eq!(merged.resolve_provider, Some(true));
        assert_eq!(merged.trigger_characters, Some(vec![".".to_string()]));
        assert!(base_first.request("textDocument/completion").is_some());
        assert!(base_first.request("completionItem/resolve").is_some());
    }

    #[test]
    fn completion_resolve_without_completion_is_a_build_error() {
        let err = Server::builder(DummyState)
            .feature(crate::features::completion_resolve(), ok_resolve)
            .build()
            .err()
            .expect("resolve without its base feature must fail");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "completionProvider"
            }
        );
    }

    #[test]
    fn unequal_resolve_contributions_within_the_family_fail() {
        let err = Server::builder(DummyState)
            .feature(
                crate::features::completion(CompletionOptions {
                    resolve_provider: Some(false),
                    ..CompletionOptions::default()
                }),
                ok_completion,
            )
            .feature(crate::features::completion_resolve(), ok_resolve)
            .build()
            .err()
            .expect("a base that denies resolve and a resolve registration clash");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "completionProvider"
            },
            "capability construction never resolves a clash by last-write-wins"
        );
    }

    async fn noop_on_initialize(
        _state: Arc<DummyState>,
        _ctx: Context,
        _params: lsp_types::InitializeParams,
        _ct: CancellationToken,
    ) -> Result<Option<lsp_types::ServerInfo>, LspError> {
        Ok(None)
    }

    #[test]
    fn duplicate_configure_initialize_is_a_build_error() {
        let err = Server::builder(DummyState)
            .configure_initialize(|_params, _registrar| Ok(()))
            .configure_initialize(|_params, _registrar| Ok(()))
            .build()
            .err()
            .expect("supplying configure_initialize twice must fail");
        assert_eq!(err, BuildError::DuplicateConfigureInitialize);
    }

    #[test]
    fn duplicate_on_initialize_is_a_build_error() {
        let err = Server::builder(DummyState)
            .on_initialize(noop_on_initialize)
            .on_initialize(noop_on_initialize)
            .build()
            .err()
            .expect("supplying on_initialize twice must fail");
        assert_eq!(err, BuildError::DuplicateLifecycleHook("on_initialize"));
    }

    async fn noop_on_initialized(
        _state: Arc<DummyState>,
        _ctx: Context,
        _params: lsp_types::InitializedParams,
    ) {
    }

    async fn noop_on_shutdown(
        _state: Arc<DummyState>,
        _ctx: Context,
        _params: (),
        _ct: CancellationToken,
    ) -> Result<(), LspError> {
        Ok(())
    }

    async fn noop_on_exit(_state: Arc<DummyState>, _ctx: Context) {}

    #[test]
    fn duplicate_on_initialized_is_a_build_error() {
        let err = Server::builder(DummyState)
            .on_initialized(noop_on_initialized)
            .on_initialized(noop_on_initialized)
            .build()
            .err()
            .expect("supplying on_initialized twice must fail");
        assert_eq!(err, BuildError::DuplicateLifecycleHook("on_initialized"));
    }

    #[test]
    fn duplicate_on_shutdown_is_a_build_error() {
        let err = Server::builder(DummyState)
            .on_shutdown(noop_on_shutdown)
            .on_shutdown(noop_on_shutdown)
            .build()
            .err()
            .expect("supplying on_shutdown twice must fail");
        assert_eq!(err, BuildError::DuplicateLifecycleHook("on_shutdown"));
    }

    #[test]
    fn duplicate_on_exit_is_a_build_error() {
        let err = Server::builder(DummyState)
            .on_exit(noop_on_exit)
            .on_exit(noop_on_exit)
            .build()
            .err()
            .expect("supplying on_exit twice must fail");
        assert_eq!(err, BuildError::DuplicateLifecycleHook("on_exit"));
    }

    #[test]
    fn lifecycle_hooks_contribute_no_catalog_capabilities() {
        let server = Server::builder(DummyState)
            .on_initialized(noop_on_initialized)
            .on_shutdown(noop_on_shutdown)
            .on_exit(noop_on_exit)
            .build()
            .expect("a server with only lifecycle hooks builds");
        let router = server.into_router();
        assert!(
            router.notification("initialized").is_none(),
            "initialized is not a Router route; it is a reserved lifecycle notification"
        );
        assert_eq!(
            router.capabilities(),
            ServerCapabilities::default(),
            "lifecycle hooks contribute nothing to the capability catalog"
        );
    }

    #[test]
    fn initialized_is_a_reserved_notification_method() {
        let err = Server::builder(DummyState)
            .notification::<lsp_types::notification::Initialized, _, _>(
                |_s, _c, _p: lsp_types::InitializedParams| async {},
            )
            .build()
            .err()
            .expect("initialized is framework-reserved");
        assert_eq!(err, BuildError::ReservedMethod("initialized".to_string()));
    }

    async fn ok_workspace_symbol(
        _state: Arc<DummyState>,
        _ctx: Context,
        _params: lsp_types::WorkspaceSymbolParams,
        _ct: CancellationToken,
    ) -> Result<Option<lsp_types::WorkspaceSymbolResponse>, LspError> {
        Ok(None)
    }

    async fn ok_symbol_resolve(
        _state: Arc<DummyState>,
        _ctx: Context,
        symbol: lsp_types::WorkspaceSymbol,
        _ct: CancellationToken,
    ) -> Result<lsp_types::WorkspaceSymbol, LspError> {
        Ok(symbol)
    }

    async fn ok_will_rename(
        _state: Arc<DummyState>,
        _ctx: Context,
        _params: lsp_types::RenameFilesParams,
        _ct: CancellationToken,
    ) -> Result<Option<lsp_types::WorkspaceEdit>, LspError> {
        Ok(None)
    }

    async fn noop_rename_files(
        _state: Arc<DummyState>,
        _ctx: Context,
        _params: lsp_types::RenameFilesParams,
    ) {
    }

    fn rename_filters() -> lsp_types::FileOperationRegistrationOptions {
        lsp_types::FileOperationRegistrationOptions {
            filters: vec![lsp_types::FileOperationFilter {
                scheme: Some("file".to_string()),
                pattern: lsp_types::FileOperationPattern {
                    glob: "**/*.rs".to_string(),
                    matches: Some(lsp_types::FileOperationPatternKind::File),
                    options: None,
                },
            }],
        }
    }

    fn workspace_symbol_options() -> lsp_types::WorkspaceSymbolOptions {
        lsp_types::WorkspaceSymbolOptions {
            work_done_progress_options: Default::default(),
            resolve_provider: None,
        }
    }

    #[test]
    fn workspace_symbol_and_resolve_merge_into_one_capability_independent_of_order() {
        let base_first = Server::builder(DummyState)
            .feature(
                crate::features::workspace_symbol(workspace_symbol_options()),
                ok_workspace_symbol,
            )
            .feature(
                crate::features::workspace_symbol_resolve(),
                ok_symbol_resolve,
            )
            .build()
            .expect("workspace symbol then resolve builds")
            .into_router();
        let resolve_first = Server::builder(DummyState)
            .feature(
                crate::features::workspace_symbol_resolve(),
                ok_symbol_resolve,
            )
            .feature(
                crate::features::workspace_symbol(workspace_symbol_options()),
                ok_workspace_symbol,
            )
            .build()
            .expect("resolve then workspace symbol builds")
            .into_router();

        assert_eq!(
            base_first.capabilities(),
            resolve_first.capabilities(),
            "the family merge is independent of registration order"
        );
        let merged = base_first
            .capabilities()
            .workspace_symbol_provider
            .expect("the family emits one workspaceSymbolProvider capability");
        let lsp_types::OneOf::Right(options) = merged else {
            panic!("the family advertises full options, not a bare boolean");
        };
        assert_eq!(options.resolve_provider, Some(true));
        assert!(base_first.request("workspace/symbol").is_some());
        assert!(base_first.request("workspaceSymbol/resolve").is_some());
    }

    #[test]
    fn workspace_symbol_resolve_without_workspace_symbol_is_a_build_error() {
        let err = Server::builder(DummyState)
            .feature(
                crate::features::workspace_symbol_resolve(),
                ok_symbol_resolve,
            )
            .build()
            .err()
            .expect("resolve without its base feature must fail");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "workspaceSymbolProvider"
            }
        );
    }

    #[test]
    fn file_operation_features_share_one_family_capability() {
        let server = Server::builder(DummyState)
            .feature(
                crate::features::will_rename_files(rename_filters()),
                ok_will_rename,
            )
            .feature_notification(
                crate::features::did_rename_files(rename_filters()),
                noop_rename_files,
            )
            .build()
            .expect("identical will/did filters merge");
        let router = server.into_router();
        assert!(router.request("workspace/willRenameFiles").is_some());
        assert!(router.notification("workspace/didRenameFiles").is_some());
        let file_operations = router
            .capabilities()
            .workspace
            .expect("the family advertises the workspace object")
            .file_operations
            .expect("the family advertises a fileOperations capability");
        let expected = Some(rename_filters());
        assert_eq!(file_operations.will_rename, expected.clone());
        assert_eq!(file_operations.did_rename, expected);
        assert_eq!(file_operations.will_create, None);
    }

    #[test]
    fn disagreeing_file_operation_filters_are_a_build_error() {
        let mut other = rename_filters();
        other.filters[0].pattern.glob = "**/*.toml".to_string();
        let err = Server::builder(DummyState)
            .feature(
                crate::features::will_rename_files(rename_filters()),
                ok_will_rename,
            )
            .feature_notification(crate::features::did_rename_files(other), noop_rename_files)
            .build()
            .err()
            .expect("differing filters within one family must fail");
        assert_eq!(
            err,
            BuildError::ConflictingCapability {
                field: "workspace.fileOperations.rename"
            }
        );
    }

    #[test]
    fn a_duplicate_notification_feature_is_a_build_error() {
        let err = Server::builder(DummyState)
            .feature_notification(
                crate::features::did_rename_files(rename_filters()),
                noop_rename_files,
            )
            .feature_notification(
                crate::features::did_rename_files(rename_filters()),
                noop_rename_files,
            )
            .build()
            .err()
            .expect("registering the same notification feature twice must fail");
        assert_eq!(
            err,
            BuildError::DuplicateMethod("workspace/didRenameFiles".to_string())
        );
    }

    #[test]
    fn watched_files_feature_registers_a_route_and_contributes_no_capability() {
        async fn noop_watched(
            _state: Arc<DummyState>,
            _ctx: Context,
            _params: lsp_types::DidChangeWatchedFilesParams,
        ) {
        }
        let server = Server::builder(DummyState)
            .feature_notification(crate::features::did_change_watched_files(), noop_watched)
            .build()
            .expect("the watched-files feature builds");
        let router = server.into_router();
        assert!(
            router
                .notification("workspace/didChangeWatchedFiles")
                .is_some(),
            "watched files is an ordinary route: the framework owns no mutation for it"
        );
        assert_eq!(
            router.capabilities(),
            ServerCapabilities::default(),
            "LSP 3.17 has no watched-files server capability, so none is advertised"
        );
    }

    #[test]
    fn the_outbound_warning_threshold_defaults_to_1024() {
        let server = Server::builder(DummyState)
            .build()
            .expect("the default threshold builds");
        assert_eq!(server.resource_policy.max_outbound_messages, 1024);
        assert_eq!(
            server.resource_policy.max_outbound_messages,
            crate::DEFAULT_OUTBOUND_WARNING_THRESHOLD
        );
    }

    #[test]
    fn the_outbound_warning_threshold_accepts_positive_values() {
        let server = Server::builder(DummyState)
            .outbound_warning_threshold(7)
            .build()
            .expect("a positive threshold builds");
        assert_eq!(server.resource_policy.max_outbound_messages, 7);
    }

    #[test]
    fn a_zero_outbound_warning_threshold_is_a_build_error() {
        let err = Server::builder(DummyState)
            .outbound_warning_threshold(0)
            .build()
            .err()
            .expect("a zero threshold must fail the build");
        assert_eq!(err, BuildError::InvalidOutboundWarningThreshold);
        assert_eq!(
            err.to_string(),
            "outbound warning threshold must be greater than zero"
        );
    }
}
