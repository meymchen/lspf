use std::fmt;
use std::time::Duration;

use crate::BuildError;

/// A field whose value made a [`ResourcePolicy`] invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourcePolicyField {
    /// [`ResourcePolicy::max_inbound_requests`].
    MaxInboundRequests,
    /// [`ResourcePolicy::max_outbound_messages`].
    MaxOutboundMessages,
    /// [`ResourcePolicy::max_outbound_bytes`].
    MaxOutboundBytes,
    /// [`ResourcePolicy::max_documents`].
    MaxDocuments,
    /// [`ResourcePolicy::max_document_bytes`].
    MaxDocumentBytes,
    /// [`ResourcePolicy::outbound_request_timeout`].
    OutboundRequestTimeout,
    /// [`ResourcePolicy::handler_timeout`].
    HandlerTimeout,
}

impl fmt::Display for ResourcePolicyField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MaxInboundRequests => "max_inbound_requests",
            Self::MaxOutboundMessages => "max_outbound_messages",
            Self::MaxOutboundBytes => "max_outbound_bytes",
            Self::MaxDocuments => "max_documents",
            Self::MaxDocumentBytes => "max_document_bytes",
            Self::OutboundRequestTimeout => "outbound_request_timeout",
            Self::HandlerTimeout => "handler_timeout",
        })
    }
}

/// Finite budgets for resources owned by one LSP connection.
///
/// A policy covers admitted inbound requests, queued outbound messages and
/// their encoded bytes, tracked Documents and their text bytes, and the two
/// connection-owned deadlines. Install a policy with
/// [`ServerBuilder::resource_policy`](crate::ServerBuilder::resource_policy).
///
/// The production defaults admit 64 inbound requests, queue 1,024 outbound
/// messages using at most 16 MiB, track 1,024 Documents using at most 64 MiB
/// of text, and apply 30-second outbound-request and handler deadlines.
/// Setting `outbound_request_timeout` to `None` explicitly disables that
/// deadline. All numeric budgets and enabled deadlines must be greater than
/// zero; invalid policies are rejected by [`ServerBuilder::build`](crate::ServerBuilder::build).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourcePolicy {
    /// Maximum admitted inbound requests for the connection.
    pub max_inbound_requests: usize,
    /// Maximum messages retained in the connection's outbound queue.
    pub max_outbound_messages: usize,
    /// Maximum encoded bytes retained in the connection's outbound queue.
    pub max_outbound_bytes: usize,
    /// Maximum Documents tracked for the connection.
    pub max_documents: usize,
    /// Maximum total text bytes retained across tracked Documents.
    pub max_document_bytes: usize,
    /// Default deadline for a server-to-client request, or `None` to disable it.
    pub outbound_request_timeout: Option<Duration>,
    /// Default deadline for an inbound request handler.
    pub handler_timeout: Duration,
}

impl ResourcePolicy {
    pub(crate) fn validate(self) -> Result<(), BuildError> {
        for (field, value) in [
            (
                ResourcePolicyField::MaxInboundRequests,
                self.max_inbound_requests,
            ),
            (
                ResourcePolicyField::MaxOutboundMessages,
                self.max_outbound_messages,
            ),
            (
                ResourcePolicyField::MaxOutboundBytes,
                self.max_outbound_bytes,
            ),
            (ResourcePolicyField::MaxDocuments, self.max_documents),
            (
                ResourcePolicyField::MaxDocumentBytes,
                self.max_document_bytes,
            ),
        ] {
            if value == 0 {
                return Err(BuildError::InvalidResourcePolicy { field });
            }
        }
        if self.outbound_request_timeout == Some(Duration::ZERO) {
            return Err(BuildError::InvalidResourcePolicy {
                field: ResourcePolicyField::OutboundRequestTimeout,
            });
        }
        if self.handler_timeout == Duration::ZERO {
            return Err(BuildError::InvalidResourcePolicy {
                field: ResourcePolicyField::HandlerTimeout,
            });
        }
        Ok(())
    }
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            max_inbound_requests: 64,
            max_outbound_messages: 1_024,
            max_outbound_bytes: 16 * 1024 * 1024,
            max_documents: 1_024,
            max_document_bytes: 64 * 1024 * 1024,
            outbound_request_timeout: Some(Duration::from_secs(30)),
            handler_timeout: Duration::from_secs(30),
        }
    }
}
