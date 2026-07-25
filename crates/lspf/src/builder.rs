//! The 0.2 connection-owned builder surface (ADR 0017).
//!
//! [`Server::builder`] collects static registrations against one application
//! state value, and [`ServerBuilder::build`] freezes them into a [`Router`]
//! without performing any I/O. This slice wires only typed custom requests;
//! standard features, notifications, commands, and the initialize transaction
//! arrive in later slices of PRD 0.2.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use lsp_types::ServerCapabilities;
use lsp_types::request::Request;
use tokio_util::sync::CancellationToken;

use crate::codec::{decode_params, encode_body};
use crate::context::Context;
use crate::error::{BuildError, LspError};

/// Method names owned by the framework's lifecycle; a custom request may not
/// shadow one of them.
const RESERVED_METHODS: &[&str] = &[
    "initialize",
    "shutdown",
    "exit",
    "initialized",
    "$/cancelRequest",
];

/// The future produced by an erased handler: its already-encoded result bytes
/// or the wire error to report. Encoding happens exactly once, inside the
/// erased handler, so the engine only moves the bytes.
type HandlerFuture = Pin<Box<dyn Future<Output = Result<Bytes, LspError>> + Send>>;

/// A type-erased custom request handler stored in the frozen [`Router`].
///
/// Its three responsibilities (ADR 0017) are to decode the incoming
/// parameters once, invoke the typed handler with native values, and encode
/// the success value once. Malformed parameters become
/// [`LspError::InvalidParams`] without ever calling the typed handler.
pub(crate) type ErasedRequestHandler<S> =
    Box<dyn Fn(Arc<S>, Context, Bytes, CancellationToken) -> HandlerFuture + Send + Sync>;

/// The permanently frozen table of user handlers for one connection
/// (ADR 0017). Built by [`ServerBuilder::build`]; no API mutates it.
pub(crate) struct Router<S> {
    requests: HashMap<String, ErasedRequestHandler<S>>,
}

impl<S> Router<S> {
    /// The erased handler registered for `method`, if any.
    pub(crate) fn request(&self, method: &str) -> Option<&ErasedRequestHandler<S>> {
        self.requests.get(method)
    }

    /// The capabilities implied by the frozen registrations. Custom requests
    /// contribute nothing, so a custom-only Router advertises the default
    /// (empty) capability set; the protocol engine layers on any
    /// protocol-owned negotiated fields separately.
    pub(crate) fn capabilities(&self) -> ServerCapabilities {
        ServerCapabilities::default()
    }
}

/// Collects static registrations for one connection before freezing them into
/// a [`Server`] (ADR 0017). Registration mistakes are recorded and surfaced by
/// [`build`](Self::build); the builder methods stay chainable.
pub struct ServerBuilder<S> {
    state: Arc<S>,
    requests: HashMap<String, ErasedRequestHandler<S>>,
    /// First registration error seen, if any. Reported by `build`.
    error: Option<BuildError>,
}

impl<S: Send + Sync + 'static> ServerBuilder<S> {
    fn new(state: S) -> Self {
        Self {
            state: Arc::new(state),
            requests: HashMap::new(),
            error: None,
        }
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
        let handler = Arc::new(handler);
        let erased: ErasedRequestHandler<S> = Box::new(move |state, ctx, params, ct| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let parsed: R::Params = decode_params(&params)?;
                let result = handler(state, ctx, parsed, ct).await?;
                encode_body(&result)
            })
        });

        if RESERVED_METHODS.contains(&method.as_str()) {
            self.record(BuildError::ReservedMethod(method));
        } else if self.requests.insert(method.clone(), erased).is_some() {
            self.record(BuildError::DuplicateMethod(method));
        }
        self
    }

    /// Validate the complete static registration set and freeze the Router.
    ///
    /// Performs no I/O and does not run any initialization callback. Returns
    /// the first [`BuildError`] recorded during registration, if any.
    pub fn build(self) -> Result<Server<S>, BuildError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(Server {
            state: self.state,
            router: Arc::new(Router {
                requests: self.requests,
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
    use lsp_types::request::{HoverRequest, Shutdown};

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
}
