//! Typed partial-result reporting for request handlers.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use gen_lsp_types::{ProgressParams, ProgressToken, RequestWithPartialResults};

use crate::client::ClientHandle;
use crate::error::ClientError;

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// A standard LSP request whose metaModel entry defines a partial result.
///
/// This trait is sealed and its implementations are generated from the
/// vendored LSP 3.18 metaModel fixture during the build.
pub trait PartialResultRequest:
    crate::types::request::Request + RequestWithPartialResults + sealed::Sealed
{
}

/// Shared request lifetime state. The mutex serializes the final report against
/// request completion, so a report is either admitted before the response or
/// rejected without racing past it.
#[derive(Clone, Debug)]
pub(crate) struct PartialResultScope {
    method: Arc<str>,
    token: ProgressToken,
    active: Arc<Mutex<bool>>,
}

impl PartialResultScope {
    pub(crate) fn new(method: String, token: ProgressToken) -> Self {
        Self {
            method: method.into(),
            token,
            active: Arc::new(Mutex::new(true)),
        }
    }

    pub(crate) fn finish(&self) {
        *self.active.lock().unwrap() = false;
    }

    pub(crate) fn method(&self) -> &str {
        &self.method
    }
}

/// A request-scoped, typed destination for partial result chunks.
///
/// Obtain this from [`ServerContext::partial_results`](crate::ServerContext::partial_results).
/// Reporting is synchronous because it admits the notification to the
/// connection's bounded outbound queue; transport I/O remains asynchronous.
pub struct PartialResultSink<'a, R: PartialResultRequest> {
    client: &'a ClientHandle,
    scope: &'a PartialResultScope,
    request: PhantomData<R>,
}

impl<'a, R: PartialResultRequest> PartialResultSink<'a, R> {
    pub(crate) fn new(client: &'a ClientHandle, scope: &'a PartialResultScope) -> Self {
        Self {
            client,
            scope,
            request: PhantomData,
        }
    }

    /// Admit one typed chunk as a `$/progress` notification.
    ///
    /// The same message and exact-byte budgets as every ordinary outbound
    /// notification apply. In particular, a full queue returns
    /// [`ClientError::OutboundOverloaded`] and the chunk is not retained.
    pub fn report(
        &self,
        chunk: <R as RequestWithPartialResults>::PartialResult,
    ) -> Result<(), ClientError> {
        let active = self.scope.active.lock().unwrap();
        if !*active {
            return Err(ClientError::InvalidHelperParams(
                "partial-result request has completed".to_string(),
            ));
        }
        let value = serde_json::to_value(chunk).map_err(ClientError::Serialize)?;
        self.client.progress(ProgressParams {
            token: self.scope.token.clone(),
            value,
        })
    }
}

include!(concat!(env!("OUT_DIR"), "/partial_result_requests.rs"));
