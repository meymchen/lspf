//! Normalized user dispatch and the fixed Service stack (ADR 0019).

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::FutureExt;
use serde_json::Value;
use tokio::sync::Semaphore;
use tracing::{Instrument, error, info_span};

use crate::builder::Router;
use crate::{Context, LspError, RequestId};

/// Whether a normalized user call came from a request or a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Request,
    Notification,
}

/// A validated, decoded call at the user Layer boundary.
///
/// Protocol framing and lifecycle state are deliberately absent. Layers can
/// inspect the stable metadata, application state, and decoded JSON value; a
/// Layer that intentionally shapes parameters can replace [`params_mut`]
/// without serializing them.
///
/// [`params_mut`]: IncomingCall::params_mut
pub struct IncomingCall<S> {
    kind: CallKind,
    method: String,
    request_id: Option<RequestId>,
    params: Value,
    context: Context,
    state: Arc<S>,
}

impl<S> IncomingCall<S> {
    pub(crate) fn request(
        method: String,
        request_id: RequestId,
        params: Value,
        context: Context,
        state: Arc<S>,
    ) -> Self {
        Self {
            kind: CallKind::Request,
            method,
            request_id: Some(request_id),
            params,
            context,
            state,
        }
    }

    pub(crate) fn notification(
        method: String,
        params: Value,
        context: Context,
        state: Arc<S>,
    ) -> Self {
        Self {
            kind: CallKind::Notification,
            method,
            request_id: None,
            params,
            context,
            state,
        }
    }

    pub fn kind(&self) -> CallKind {
        self.kind
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn request_id(&self) -> Option<&RequestId> {
        self.request_id.as_ref()
    }

    pub fn params(&self) -> &Value {
        &self.params
    }

    pub fn params_mut(&mut self) -> &mut Value {
        &mut self.params
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn state(&self) -> &Arc<S> {
        &self.state
    }
}

/// The only outcomes of normalized user dispatch.
pub enum ServiceResult {
    Response(Value),
    Error(LspError),
    NoResponse,
}

/// Boxed future returned by a user [`Layer`].
pub type ServiceFuture = Pin<Box<dyn Future<Output = ServiceResult> + Send + 'static>>;

pub(crate) trait Service<S>: Send + Sync {
    fn call(&self, call: IncomingCall<S>) -> ServiceFuture;
}

/// The next inner Service in a user Layer chain.
pub struct Next<S> {
    inner: Arc<dyn Service<S>>,
}

impl<S> Clone for Next<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S: Send + Sync + 'static> Next<S> {
    pub fn call(&self, call: IncomingCall<S>) -> ServiceFuture {
        self.inner.call(call)
    }
}

/// Adds cross-cutting behavior around normalized user dispatch.
///
/// The last Layer registered on [`ServerBuilder`](crate::ServerBuilder) is
/// outermost among user Layers. Framework panic isolation, tracing, and
/// concurrency limiting always remain outside every user Layer.
pub trait Layer<S>: Send + Sync + 'static {
    fn call(&self, call: IncomingCall<S>, next: Next<S>) -> ServiceFuture;
}

pub(crate) type UserLayer<S> = Arc<dyn Layer<S>>;
pub(crate) type UserService<S> = Arc<dyn Service<S>>;

struct LayerService<S> {
    layer: UserLayer<S>,
    inner: UserService<S>,
}

impl<S: Send + Sync + 'static> Service<S> for LayerService<S> {
    fn call(&self, call: IncomingCall<S>) -> ServiceFuture {
        self.layer.call(
            call,
            Next {
                inner: Arc::clone(&self.inner),
            },
        )
    }
}

struct RouterService<S> {
    router: Arc<Router<S>>,
}

impl<S: Send + Sync + 'static> Service<S> for RouterService<S> {
    fn call(&self, call: IncomingCall<S>) -> ServiceFuture {
        let router = Arc::clone(&self.router);
        Box::pin(async move {
            match call.kind {
                CallKind::Request => {
                    let cancellation = call
                        .context
                        .cancellation()
                        .cloned()
                        .expect("request contexts carry cancellation");
                    let result = if call.method == "workspace/executeCommand"
                        && router.has_commands()
                    {
                        let params: lsp_types::ExecuteCommandParams =
                            match serde_json::from_value(call.params) {
                                Ok(params) => params,
                                Err(error) => {
                                    return ServiceResult::Error(LspError::invalid_params(error));
                                }
                            };
                        match router.command(&params.command) {
                            Some(handler) => {
                                handler(call.state, call.context, params.arguments, cancellation)
                                    .await
                            }
                            None => Err(LspError::invalid_params(format!(
                                "unknown command: {}",
                                params.command
                            ))),
                        }
                    } else {
                        match router.request(&call.method) {
                            Some(handler) => {
                                handler(call.state, call.context, call.params, cancellation).await
                            }
                            None => Err(LspError::MethodNotFound(call.method)),
                        }
                    };
                    match result {
                        Ok(value) => ServiceResult::Response(value),
                        Err(error) => ServiceResult::Error(error),
                    }
                }
                CallKind::Notification => {
                    // A built-in document notification reaches the stack only
                    // once the engine has decoded and mutated; its hook lives
                    // in a table registration keeps disjoint from the routes
                    // (ADR 0018), so at most one of these two lookups can ever
                    // match and their order carries no meaning.
                    let handler = router
                        .notification(&call.method)
                        .or_else(|| router.document_hook(&call.method));
                    if let Some(handler) = handler {
                        handler(call.state, call.context, call.params).await;
                    }
                    ServiceResult::NoResponse
                }
            }
        })
    }
}

struct ConcurrencyLimitService<S> {
    inner: UserService<S>,
    permits: Arc<Semaphore>,
}

impl<S: Send + Sync + 'static> Service<S> for ConcurrencyLimitService<S> {
    fn call(&self, call: IncomingCall<S>) -> ServiceFuture {
        let inner = Arc::clone(&self.inner);
        let permits = Arc::clone(&self.permits);
        Box::pin(async move {
            let _permit = permits
                .acquire_owned()
                .instrument(info_span!("handler.acquire_permit"))
                .await
                .expect("service semaphore is never closed");
            inner.call(call).await
        })
    }
}

struct TracingService<S> {
    inner: UserService<S>,
}

impl<S: Send + Sync + 'static> Service<S> for TracingService<S> {
    fn call(&self, call: IncomingCall<S>) -> ServiceFuture {
        let inner = Arc::clone(&self.inner);
        let span = call.context.span().clone();
        Box::pin(inner.call(call).instrument(span))
    }
}

struct PanicIsolationService<S> {
    inner: UserService<S>,
}

impl<S: Send + Sync + 'static> Service<S> for PanicIsolationService<S> {
    fn call(&self, call: IncomingCall<S>) -> ServiceFuture {
        let inner = Arc::clone(&self.inner);
        let kind = call.kind;
        Box::pin(async move {
            match AssertUnwindSafe(async move { inner.call(call).await })
                .catch_unwind()
                .await
            {
                Ok(result) => result,
                Err(_) if kind == CallKind::Request => {
                    error!("panic isolated while dispatching request");
                    ServiceResult::Error(LspError::internal("user dispatch panicked"))
                }
                Err(_) => {
                    error!("panic isolated while dispatching notification");
                    ServiceResult::NoResponse
                }
            }
        })
    }
}

pub(crate) fn build_service_stack<S>(
    router: Arc<Router<S>>,
    layers: Vec<UserLayer<S>>,
    concurrency_limit: usize,
) -> UserService<S>
where
    S: Send + Sync + 'static,
{
    let mut service: UserService<S> = Arc::new(RouterService { router });
    for layer in layers {
        service = Arc::new(LayerService {
            layer,
            inner: service,
        });
    }
    service = Arc::new(ConcurrencyLimitService {
        inner: service,
        permits: Arc::new(Semaphore::new(concurrency_limit)),
    });
    service = Arc::new(TracingService { inner: service });
    Arc::new(PanicIsolationService { inner: service })
}
