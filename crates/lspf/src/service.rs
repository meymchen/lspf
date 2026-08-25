//! Normalized user dispatch and the fixed Service stack (ADR 0019).

use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

use futures_util::FutureExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info_span};

use crate::builder::Router;
use crate::sync::Semaphore;
use crate::telemetry::{Deadline, DeadlineAction, Direction};
use crate::{Context, LspError, RequestId, TaskFuture, TaskSend};

/// Whether a normalized user call came from a request or a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// A call that must resolve to a response or error.
    Request,
    /// A fire-and-forget call.
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
    handler_timeout: Option<HandlerTimeout>,
}

#[derive(Clone)]
pub(crate) struct HandlerTimeout {
    duration: Arc<Mutex<Duration>>,
    armed: CancellationToken,
    trace: crate::telemetry::ConnectionTrace,
    method: Arc<str>,
    request_id: RequestId,
    deadline_started: Arc<Mutex<Option<Instant>>>,
    finished: Arc<AtomicBool>,
}

impl HandlerTimeout {
    pub(crate) fn new(
        timeout: Duration,
        trace: crate::telemetry::ConnectionTrace,
        method: impl Into<Arc<str>>,
        request_id: RequestId,
    ) -> Self {
        Self {
            duration: Arc::new(Mutex::new(timeout)),
            armed: CancellationToken::new(),
            trace,
            method: method.into(),
            request_id,
            deadline_started: Arc::new(Mutex::new(None)),
            finished: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn get(&self) -> Duration {
        *self.duration.lock().unwrap()
    }

    fn set(&self, timeout: Duration) {
        *self.duration.lock().unwrap() = timeout;
    }

    fn arm(&self) {
        *self.deadline_started.lock().unwrap() = Some(Instant::now());
        self.trace.deadline(
            Deadline::Handler,
            DeadlineAction::Armed,
            Direction::Inbound,
            &self.method,
            &self.request_id,
            self.get(),
            Duration::ZERO,
        );
        self.armed.cancel();
    }

    pub(crate) fn finish(&self, action: DeadlineAction) {
        let Some(started) = *self.deadline_started.lock().unwrap() else {
            return;
        };
        if self.finished.swap(true, Ordering::Relaxed) {
            return;
        }
        let limit = self.get();
        self.trace.deadline(
            Deadline::Handler,
            action,
            Direction::Inbound,
            &self.method,
            &self.request_id,
            limit,
            if matches!(action, DeadlineAction::Expired) {
                limit
            } else {
                started.elapsed()
            },
        );
    }

    pub(crate) async fn wait_until_armed(&self) {
        self.armed.cancelled().await;
    }
}

impl<S> IncomingCall<S> {
    pub(crate) fn request(
        method: String,
        request_id: RequestId,
        params: Value,
        context: Context,
        state: Arc<S>,
        handler_timeout: HandlerTimeout,
    ) -> Self {
        Self {
            kind: CallKind::Request,
            method,
            request_id: Some(request_id),
            params,
            context,
            state,
            handler_timeout: Some(handler_timeout),
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
            handler_timeout: None,
        }
    }

    /// Whether this call is a request or notification.
    pub fn kind(&self) -> CallKind {
        self.kind
    }

    /// The LSP or custom method name.
    pub fn method(&self) -> &str {
        &self.method
    }

    /// The JSON-RPC request ID, or `None` for a notification.
    pub fn request_id(&self) -> Option<&RequestId> {
        self.request_id.as_ref()
    }

    /// The decoded JSON parameters.
    pub fn params(&self) -> &Value {
        &self.params
    }

    /// Mutably borrow the decoded parameters before forwarding the call.
    pub fn params_mut(&mut self) -> &mut Value {
        &mut self.params
    }

    /// The framework context for this call.
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// The connection's shared application state.
    pub fn state(&self) -> &Arc<S> {
        &self.state
    }

    /// The configured timeout for this request, or `None` for a notification.
    ///
    /// Requests begin with the connection's
    /// [`ResourcePolicy::handler_timeout`](crate::ResourcePolicy::handler_timeout).
    /// A Layer may replace it with [`set_handler_timeout`](Self::set_handler_timeout)
    /// before forwarding the call.
    pub fn handler_timeout(&self) -> Option<Duration> {
        self.handler_timeout.as_ref().map(HandlerTimeout::get)
    }

    /// Override the timeout applied to this request handler.
    ///
    /// Call this synchronously before forwarding the call through [`Next`].
    /// A zero timeout expires the request as soon as dispatch is first polled.
    /// Notifications have no response deadline, so calling this for a
    /// notification has no effect.
    pub fn set_handler_timeout(&mut self, timeout: Duration) {
        if let Some(handler_timeout) = &self.handler_timeout {
            handler_timeout.set(timeout);
        }
    }
}

/// The only outcomes of normalized user dispatch.
pub enum ServiceResult {
    /// A successful request result.
    Response(Value),
    /// A request error.
    Error(LspError),
    /// No response, as required for notifications.
    NoResponse,
}

/// Boxed future returned by a user [`Layer`].
pub type ServiceFuture = Pin<Box<dyn TaskFuture<ServiceResult> + 'static>>;

macro_rules! service_trait {
    ($($native_bound:tt)*) => {
        pub(crate) trait Service<S>: TaskSend $($native_bound)* {
            fn call(&self, call: IncomingCall<S>) -> ServiceFuture;
        }
    };
}

#[cfg(not(target_arch = "wasm32"))]
service_trait!(+ Sync);
#[cfg(target_arch = "wasm32")]
service_trait!();

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
    /// Forward `call` to the next inner Layer or the Router.
    pub fn call(&self, call: IncomingCall<S>) -> ServiceFuture {
        self.inner.call(call)
    }
}

macro_rules! layer_trait {
    ($($native_bound:tt)*) => {
        /// Adds cross-cutting behavior around normalized user dispatch.
        ///
        /// The last Layer registered on [`ServerBuilder`](crate::ServerBuilder) is
        /// outermost among user Layers. Framework panic isolation, tracing, and
        /// concurrency limiting always remain outside every user Layer.
        pub trait Layer<S>: TaskSend $($native_bound)* + 'static {
            /// Process `call`, optionally forwarding it through `next`.
            fn call(&self, call: IncomingCall<S>, next: Next<S>) -> ServiceFuture;
        }
    };
}

#[cfg(not(target_arch = "wasm32"))]
layer_trait!(+ Sync);
#[cfg(target_arch = "wasm32")]
layer_trait!();

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
                                handler
                                    .invoke((
                                        call.state,
                                        call.context,
                                        params.arguments,
                                        cancellation,
                                    ))
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
                                handler
                                    .invoke((call.state, call.context, call.params, cancellation))
                                    .await
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
                        .or_else(|| router.built_in_hook(&call.method));
                    if let Some(handler) = handler {
                        handler
                            .invoke((call.state, call.context, call.params))
                            .await;
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
        let handler_timeout = call.handler_timeout.clone();
        Box::pin(async move {
            let _permit = permits
                .clone()
                .acquire_owned()
                .instrument(info_span!("handler.acquire_permit"))
                .await;
            let handler = inner.call(call);
            if let Some(handler_timeout) = handler_timeout {
                handler_timeout.arm();
            }
            handler.await
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
        permits: Semaphore::shared(concurrency_limit),
    });
    service = Arc::new(TracingService { inner: service });
    Arc::new(PanicIsolationService { inner: service })
}
