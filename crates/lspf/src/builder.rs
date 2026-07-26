//! The 0.2 connection-owned builder surface (ADR 0017).
//!
//! [`Server::builder`] collects static registrations against one application
//! state value, and [`ServerBuilder::build`] freezes them into a [`Router`]
//! without performing any I/O. This surface wires typed custom requests and
//! notifications, typed commands beneath `workspace/executeCommand`, and the
//! standard hover and completion features. The initialize transaction and the
//! remaining catalog features arrive in later slices of PRD 0.2.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use lsp_types::ServerCapabilities;
use lsp_types::notification::Notification;
use lsp_types::request::Request;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::capability::CapabilityBuilder;
use crate::codec::{decode_params, encode_body};
use crate::context::Context;
use crate::error::{BuildError, LspError};
use crate::features::FeatureSpec;

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

/// The future produced by an erased request or command handler: its
/// already-encoded result bytes or the wire error to report. Encoding happens
/// exactly once, inside the erased handler, so the engine only moves the bytes.
type HandlerFuture = Pin<Box<dyn Future<Output = Result<Bytes, LspError>> + Send>>;

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
    Box<dyn Fn(Arc<S>, Context, Bytes, CancellationToken) -> HandlerFuture + Send + Sync>;

/// A type-erased notification handler stored in the frozen [`Router`].
///
/// Like the request handler it decodes once and invokes the typed handler, but
/// it encodes nothing: notifications have no response. Malformed parameters are
/// logged and dropped without ever calling the typed handler.
pub(crate) type ErasedNotificationHandler<S> =
    Box<dyn Fn(Arc<S>, Context, Bytes) -> NotificationFuture + Send + Sync>;

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
            let parsed: R::Params = decode_params(&params)?;
            let result = handler(state, ctx, parsed, ct).await?;
            encode_body(&result)
        })
    })
}

/// The permanently frozen table of user handlers for one connection
/// (ADR 0017). Built by [`ServerBuilder::build`]; no API mutates it.
pub(crate) struct Router<S> {
    requests: HashMap<String, ErasedRequestHandler<S>>,
    notifications: HashMap<String, ErasedNotificationHandler<S>>,
    commands: HashMap<String, ErasedCommandHandler<S>>,
    /// Capabilities implied by the frozen registrations, computed once at
    /// build time from the same registrations used for dispatch.
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

/// Collects static registrations for one connection before freezing them into
/// a [`Server`] (ADR 0017). Registration mistakes are recorded and surfaced by
/// [`build`](Self::build); the builder methods stay chainable.
pub struct ServerBuilder<S> {
    state: Arc<S>,
    requests: HashMap<String, ErasedRequestHandler<S>>,
    notifications: HashMap<String, ErasedNotificationHandler<S>>,
    commands: HashMap<String, ErasedCommandHandler<S>>,
    capabilities: CapabilityBuilder,
    /// First registration error seen, if any. Reported by `build`.
    error: Option<BuildError>,
}

impl<S: Send + Sync + 'static> ServerBuilder<S> {
    fn new(state: S) -> Self {
        Self {
            state: Arc::new(state),
            requests: HashMap::new(),
            notifications: HashMap::new(),
            commands: HashMap::new(),
            capabilities: CapabilityBuilder::default(),
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
        let method = <F::Marker as Request>::METHOD.to_string();
        let erased = erase_request::<S, F::Marker, H, Fut>(handler);

        if RESERVED_METHODS.contains(&method.as_str()) {
            self.record(BuildError::ReservedMethod(method));
        } else if self.requests.insert(method.clone(), erased).is_some() {
            self.record(BuildError::DuplicateMethod(method));
        } else if let Err(err) = spec.contribute(&mut self.capabilities) {
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
        let method = R::METHOD.to_string();
        let erased = erase_request::<S, R, H, Fut>(handler);

        if RESERVED_METHODS.contains(&method.as_str()) {
            self.record(BuildError::ReservedMethod(method));
        } else if self.requests.insert(method.clone(), erased).is_some() {
            self.record(BuildError::DuplicateMethod(method));
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
        let method = N::METHOD.to_string();
        let handler = Arc::new(handler);
        let erased: ErasedNotificationHandler<S> = Box::new(move |state, ctx, params| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let parsed: N::Params = match decode_params(&params) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        // A notification has no reply, so a decode failure is
                        // reported through tracing and dropped; later messages
                        // are unaffected (ADR 0017).
                        warn!(
                            method = N::METHOD,
                            error = %err,
                            "dropping notification with malformed params"
                        );
                        return;
                    }
                };
                handler(state, ctx, parsed).await;
            })
        });

        if RESERVED_METHODS.contains(&method.as_str()) {
            self.record(BuildError::ReservedMethod(method));
        } else if self.notifications.insert(method.clone(), erased).is_some() {
            self.record(BuildError::DuplicateMethod(method));
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
        let name = name.into();
        let handler = Arc::new(handler);
        let erased: ErasedCommandHandler<S> = Box::new(move |state, ctx, arguments, ct| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let args: Args = serde_json::from_value(Value::Array(arguments))
                    .map_err(LspError::invalid_params)?;
                let result = handler(state, ctx, args, ct).await?;
                encode_body(&result)
            })
        });

        if name.is_empty() {
            self.record(BuildError::EmptyCommandName);
        } else if self.commands.insert(name.clone(), erased).is_some() {
            self.record(BuildError::DuplicateCommand(name));
        } else {
            self.capabilities.add_command(name);
        }
        self
    }

    /// Validate the complete static registration set and freeze the Router.
    ///
    /// Performs no I/O and does not run any initialization callback. Returns
    /// the first [`BuildError`] recorded during registration, if any.
    pub fn build(mut self) -> Result<Server<S>, BuildError> {
        // A command registration and an explicit `workspace/executeCommand`
        // request handler both claim the same method; they cannot coexist.
        if !self.commands.is_empty() && self.requests.contains_key(EXECUTE_COMMAND_METHOD) {
            self.record(BuildError::ExecuteCommandConflict);
        }
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(Server {
            state: self.state,
            router: Arc::new(Router {
                requests: self.requests,
                notifications: self.notifications,
                commands: self.commands,
                capabilities: self.capabilities.finish(),
            }),
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

/// Owns exactly one LSP connection and its frozen [`Router`] (ADR 0017). A
/// second connection requires a second `Server`; connection state is never
/// shared between servers.
pub struct Server<S> {
    pub(crate) state: Arc<S>,
    pub(crate) router: Arc<Router<S>>,
}

impl<S: Send + Sync + 'static> Server<S> {
    /// Begin building a connection-owned server around one application state
    /// value. Every handler for the connection shares `state` as `Arc<S>`.
    pub fn builder(state: S) -> ServerBuilder<S> {
        ServerBuilder::new(state)
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
        assert!(server.router.request("textDocument/hover").is_some());
        assert!(server.router.request("nope").is_none());
        assert_eq!(
            server.router.capabilities(),
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
        assert!(
            server
                .router
                .notification("workspace/didChangeConfiguration")
                .is_some()
        );
        assert_eq!(server.router.capabilities(), ServerCapabilities::default());
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
            .router
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
        let caps = server.router.capabilities();
        assert_eq!(
            caps.hover_provider,
            Some(HoverProviderCapability::Simple(true))
        );
        assert_eq!(caps.completion_provider, None);
        assert!(server.router.request("textDocument/hover").is_some());
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
            .router
            .capabilities();
        let completion_first = Server::builder(DummyState)
            .feature(crate::features::completion(options.clone()), ok_completion)
            .feature(crate::features::hover(), ok_hover)
            .build()
            .expect("completion then hover builds")
            .router
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
}
