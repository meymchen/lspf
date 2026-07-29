//! The 0.2 connection-owned builder surface (ADR 0017, ADR 0018).
//!
//! [`Server::builder`] collects static registrations against one application
//! state value; [`ServerBuilder::build`] validates them and returns a [`Server`]
//! without performing any I/O or freezing the [`Router`]. The protocol engine
//! freezes the Router later, when it commits the initialize transaction: after a
//! valid `initialize`, it runs the sole [`configure_initialize`] callback
//! against a transactional [`InitializeRegistrar`], then the [`on_initialize`]
//! lifecycle hook. This surface wires typed custom requests and notifications,
//! typed commands beneath `workspace/executeCommand`, the standard hover and
//! completion features, and the two initialize-time hooks. The remaining catalog
//! features arrive in later slices of PRD 0.2.
//!
//! [`configure_initialize`]: ServerBuilder::configure_initialize
//! [`on_initialize`]: ServerBuilder::on_initialize

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lsp_types::notification::Notification;
use lsp_types::request::Request;
use lsp_types::{InitializeParams, ServerCapabilities, ServerInfo};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::capability::CapabilityBuilder;
use crate::codec::erase_value;
use crate::context::Context;
use crate::error::{BuildError, LspError};
use crate::features::FeatureSpec;
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

/// The future produced by an erased request or command handler: its decoded,
/// method-erased result or the error to report.
type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, LspError>> + Send>>;

/// The future produced by an erased notification handler. A notification has no
/// response, so it resolves to `()`; when decoding fails the future logs the
/// error and returns without invoking the typed handler.
type NotificationFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A type-erased custom request handler stored in the frozen [`Router`].
///
/// Its three responsibilities (ADR 0017) are to decode the incoming
/// parameters once, invoke the typed handler with native values, and encode
/// the success value once. Malformed parameters become
/// [`LspError::InvalidParams`] without ever calling the typed handler.
pub(crate) type ErasedRequestHandler<S> =
    Box<dyn Fn(Arc<S>, Context, Value, CancellationToken) -> HandlerFuture + Send + Sync>;

/// A type-erased notification handler stored in the frozen [`Router`].
///
/// Like the request handler it decodes once and invokes the typed handler, but
/// it encodes nothing: notifications have no response. Malformed parameters are
/// logged and dropped without ever calling the typed handler.
pub(crate) type ErasedNotificationHandler<S> =
    Box<dyn Fn(Arc<S>, Context, Value) -> NotificationFuture + Send + Sync>;

/// A type-erased command handler stored in the frozen [`Router`].
///
/// The engine decodes `workspace/executeCommand`'s [`ExecuteCommandParams`] to
/// route by command name, then hands the raw argument array here. The erased
/// handler decodes those arguments into the typed `Args` once, invokes the
/// typed handler, and encodes its `Output` once.
///
/// [`ExecuteCommandParams`]: lsp_types::ExecuteCommandParams
pub(crate) type ErasedCommandHandler<S> =
    Box<dyn Fn(Arc<S>, Context, Vec<Value>, CancellationToken) -> HandlerFuture + Send + Sync>;

/// The synchronous, run-at-most-once initialization-dependent registration
/// callback (ADR 0017). It receives read-only `InitializeParams` and a
/// transactional [`InitializeRegistrar`]; returning `Err` discards the whole
/// transaction. Boxed `FnOnce` because the engine invokes it exactly once.
pub(crate) type ConfigureInitialize<S> =
    Box<dyn FnOnce(&InitializeParams, &mut InitializeRegistrar<S>) -> Result<(), LspError> + Send>;

/// The future produced by the erased `on_initialize` hook: optional
/// [`ServerInfo`] to combine with the generated capabilities, or an
/// [`LspError`] that fails initialization.
type OnInitializeFuture =
    Pin<Box<dyn Future<Output = Result<Option<ServerInfo>, LspError>> + Send>>;

/// The erased `on_initialize` lifecycle hook (ADR 0018). It has the request
/// handler shape but returns optional [`ServerInfo`]; it cannot register routes
/// or replace the generated capabilities.
pub(crate) type OnInitialize<S> = Box<
    dyn Fn(Arc<S>, Context, InitializeParams, CancellationToken) -> OnInitializeFuture
        + Send
        + Sync,
>;

/// Wrap a typed request handler in the erased closure the [`Router`] stores.
/// Shared by [`ServerBuilder::request`] and [`ServerBuilder::feature`], which
/// differ only in whether the method also contributes a capability.
fn erase_request<S, R, H, Fut>(handler: H) -> ErasedRequestHandler<S>
where
    S: Send + Sync + 'static,
    R: Request,
    H: Fn(Arc<S>, Context, R::Params, CancellationToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<R::Result, LspError>> + Send + 'static,
{
    let handler = Arc::new(handler);
    Box::new(move |state, ctx, params, ct| {
        let handler = Arc::clone(&handler);
        Box::pin(async move {
            let parsed: R::Params =
                serde_json::from_value(params).map_err(LspError::invalid_params)?;
            let result = handler(state, ctx, parsed, ct).await?;
            erase_value(result)
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
    commands: HashMap<String, ErasedCommandHandler<S>>,
    capabilities: CapabilityBuilder,
}

impl<S: Send + Sync + 'static> Registrations<S> {
    fn new() -> Self {
        Self {
            requests: HashMap::new(),
            notifications: HashMap::new(),
            commands: HashMap::new(),
            capabilities: CapabilityBuilder::default(),
        }
    }

    /// Register a standard feature handler and its capability contribution.
    fn add_feature<F, H, Fut>(&mut self, spec: F, handler: H) -> Result<(), BuildError>
    where
        F: FeatureSpec,
        H: Fn(Arc<S>, Context, <F::Marker as Request>::Params, CancellationToken) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<<F::Marker as Request>::Result, LspError>> + Send + 'static,
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
        H: Fn(Arc<S>, Context, R::Params, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R::Result, LspError>> + Send + 'static,
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
        H: Fn(Arc<S>, Context, N::Params) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let method = N::METHOD.to_string();
        let handler = Arc::new(handler);
        let erased: ErasedNotificationHandler<S> = Box::new(move |state, ctx, params| {
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
                handler(state, ctx, parsed).await;
            })
        });

        if RESERVED_METHODS.contains(&method.as_str()) {
            return Err(BuildError::ReservedMethod(method));
        }
        if self.notifications.insert(method.clone(), erased).is_some() {
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
        Args: DeserializeOwned + Send + 'static,
        Output: Serialize + 'static,
        H: Fn(Arc<S>, Context, Args, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Output, LspError>> + Send + 'static,
    {
        if name.is_empty() {
            return Err(BuildError::EmptyCommandName);
        }
        let handler = Arc::new(handler);
        let erased: ErasedCommandHandler<S> = Box::new(move |state, ctx, arguments, ct| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let args: Args = serde_json::from_value(Value::Array(arguments))
                    .map_err(LspError::invalid_params)?;
                let result = handler(state, ctx, args, ct).await?;
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
        // A command registration and an explicit `workspace/executeCommand`
        // request handler both claim the same method; they cannot coexist.
        if !self.commands.is_empty() && self.requests.contains_key(EXECUTE_COMMAND_METHOD) {
            return Err(BuildError::ExecuteCommandConflict);
        }
        Ok(())
    }

    /// Freeze the registrations into the connection's permanent [`Router`],
    /// computing its capability catalog once from the same registrations used
    /// for dispatch (ADR 0017).
    pub(crate) fn freeze(self) -> Router<S> {
        Router {
            requests: self.requests,
            notifications: self.notifications,
            commands: self.commands,
            capabilities: self.capabilities.finish(),
        }
    }
}

/// The permanently frozen table of user handlers for one connection
/// (ADR 0017). The protocol engine produces it by freezing [`Registrations`]
/// once the initialize transaction commits; no API mutates it afterward.
pub(crate) struct Router<S> {
    requests: HashMap<String, ErasedRequestHandler<S>>,
    notifications: HashMap<String, ErasedNotificationHandler<S>>,
    commands: HashMap<String, ErasedCommandHandler<S>>,
    /// Capabilities implied by the frozen registrations, computed once at
    /// freeze time from the same registrations used for dispatch.
    capabilities: ServerCapabilities,
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
    pub(crate) fn capabilities(&self) -> ServerCapabilities {
        self.capabilities.clone()
    }
}

/// Collects static registrations for one connection before handing them to a
/// [`Server`] (ADR 0017). Registration mistakes are recorded and surfaced by
/// [`build`](Self::build); the builder methods stay chainable.
pub struct ServerBuilder<S> {
    state: Arc<S>,
    registrations: Registrations<S>,
    configure_initialize: Option<ConfigureInitialize<S>>,
    on_initialize: Option<OnInitialize<S>>,
    layers: Vec<UserLayer<S>>,
    concurrency_limit: usize,
    /// First registration error seen, if any. Reported by `build`.
    error: Option<BuildError>,
}

impl<S: Send + Sync + 'static> ServerBuilder<S> {
    fn new(state: S) -> Self {
        Self {
            state: Arc::new(state),
            registrations: Registrations::new(),
            configure_initialize: None,
            on_initialize: None,
            layers: Vec::new(),
            concurrency_limit: crate::DEFAULT_CONCURRENCY_LIMIT,
            error: None,
        }
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
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<<F::Marker as Request>::Result, LspError>> + Send + 'static,
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
        H: Fn(Arc<S>, Context, R::Params, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R::Result, LspError>> + Send + 'static,
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
        H: Fn(Arc<S>, Context, N::Params) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        if let Err(err) = self.registrations.add_notification::<N, H, Fut>(handler) {
            self.record(err);
        }
        self
    }

    /// Register a typed command dispatched on `workspace/executeCommand`.
    ///
    /// The command is invoked when the editor sends `workspace/executeCommand`
    /// with a matching `name`; its `arguments` array is decoded into `Args`,
    /// and the handler's `Output` is returned as the command result. The
    /// handler receives the shared application state, a [`Context`], the typed
    /// arguments, and a request-scoped [`CancellationToken`]. `Args` and
    /// `Output` are bounded by the serialization required to cross the wire.
    ///
    /// Each registered `name` merges into one deterministic execute-command
    /// capability (ADR 0017). An empty name, two handlers for the same name, or
    /// a command alongside an explicit `workspace/executeCommand`
    /// [`request`](Self::request) handler is a [`BuildError`] reported by
    /// [`build`](Self::build).
    pub fn command<Args, Output, H, Fut>(mut self, name: impl Into<String>, handler: H) -> Self
    where
        Args: DeserializeOwned + Send + 'static,
        Output: Serialize + 'static,
        H: Fn(Arc<S>, Context, Args, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Output, LspError>> + Send + 'static,
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
            + Send
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
        H: Fn(Arc<S>, Context, InitializeParams, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<ServerInfo>, LspError>> + Send + 'static,
    {
        if self.on_initialize.is_some() {
            self.record(BuildError::DuplicateLifecycleHook("on_initialize"));
        } else {
            // The hook runs once, so — unlike the many-shot request handlers —
            // it needs no `Arc`; the erasing closure just boxes its future.
            self.on_initialize = Some(Box::new(move |state, ctx, params, ct| {
                Box::pin(hook(state, ctx, params, ct))
            }));
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

    /// Set the maximum number of calls executing inside the complete user
    /// Layer chain. Zero is rejected by [`build`](Self::build).
    pub fn concurrency_limit(mut self, limit: usize) -> Self {
        if limit == 0 {
            self.record(BuildError::InvalidConcurrencyLimit);
        } else {
            self.concurrency_limit = limit;
        }
        self
    }

    /// Validate the complete static registration set and return the [`Server`].
    ///
    /// Performs no I/O and does not run `configure_initialize`; the Router is
    /// frozen later, when the engine commits the initialize transaction. Returns
    /// the first [`BuildError`] recorded during registration, if any.
    pub fn build(mut self) -> Result<Server<S>, BuildError> {
        if let Err(err) = self.registrations.validate() {
            self.record(err);
        }
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(Server {
            state: self.state,
            registrations: self.registrations,
            configure_initialize: self.configure_initialize,
            on_initialize: self.on_initialize,
            layers: self.layers,
            concurrency_limit: self.concurrency_limit,
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
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<<F::Marker as Request>::Result, LspError>> + Send + 'static,
    {
        self.try_register(|r| r.add_feature(spec, handler))
    }

    /// Conditionally register a typed custom request. See
    /// [`ServerBuilder::request`].
    pub fn request<R, H, Fut>(&mut self, handler: H) -> &mut Self
    where
        R: Request,
        H: Fn(Arc<S>, Context, R::Params, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R::Result, LspError>> + Send + 'static,
    {
        self.try_register(|r| r.add_request::<R, H, Fut>(handler))
    }

    /// Conditionally register a typed custom notification. See
    /// [`ServerBuilder::notification`].
    pub fn notification<N, H, Fut>(&mut self, handler: H) -> &mut Self
    where
        N: Notification,
        H: Fn(Arc<S>, Context, N::Params) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.try_register(|r| r.add_notification::<N, H, Fut>(handler))
    }

    /// Conditionally register a typed command. See [`ServerBuilder::command`].
    pub fn command<Args, Output, H, Fut>(
        &mut self,
        name: impl Into<String>,
        handler: H,
    ) -> &mut Self
    where
        Args: DeserializeOwned + Send + 'static,
        Output: Serialize + 'static,
        H: Fn(Arc<S>, Context, Args, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Output, LspError>> + Send + 'static,
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
/// registrations awaiting the initialize transaction, and the optional
/// initialization-dependent callback and `on_initialize` hook (ADR 0017,
/// ADR 0018). A second connection requires a second `Server`; connection state
/// is never shared between servers.
pub struct Server<S> {
    pub(crate) state: Arc<S>,
    pub(crate) registrations: Registrations<S>,
    pub(crate) configure_initialize: Option<ConfigureInitialize<S>>,
    pub(crate) on_initialize: Option<OnInitialize<S>>,
    pub(crate) layers: Vec<UserLayer<S>>,
    pub(crate) concurrency_limit: usize,
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
    /// Returns when the peer sends `exit`, closes the transport, or a
    /// transport error ends the connection.
    pub async fn serve<T>(self, transport: T) -> crate::Result<()>
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
    fn custom_notifications_contribute_no_capabilities() {
        let server = Server::builder(DummyState)
            .notification::<lsp_types::notification::DidChangeConfiguration, _, _>(
                |_s, _c, _p: lsp_types::DidChangeConfigurationParams| async {},
            )
            .build()
            .expect("a lone notification builds");
        let router = server.into_router();
        assert!(
            router
                .notification("workspace/didChangeConfiguration")
                .is_some()
        );
        assert_eq!(router.capabilities(), ServerCapabilities::default());
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
            vec!["a.cmd".to_string(), "b.cmd".to_string()],
            "command names merge into one sorted, order-independent list"
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
}
