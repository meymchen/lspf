use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tracing::{Span, info_span, trace};

use crate::{RawMessage, RequestId};

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Resource {
    InboundRequests,
    OutboundQueue,
    Documents,
    PendingRequests,
}

impl Resource {
    fn as_str(self) -> &'static str {
        match self {
            Self::InboundRequests => "inbound_requests",
            Self::OutboundQueue => "outbound_queue",
            Self::Documents => "documents",
            Self::PendingRequests => "pending_requests",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ResourceAction {
    Admit,
    Release,
    Update,
    Reject,
    Rollback,
}

impl ResourceAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::Release => "release",
            Self::Update => "update",
            Self::Reject => "reject",
            Self::Rollback => "rollback",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Deadline {
    Handler,
    OutboundRequest,
}

impl Deadline {
    fn as_str(self) -> &'static str {
        match self {
            Self::Handler => "handler",
            Self::OutboundRequest => "outbound_request",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum DeadlineAction {
    Armed,
    Completed,
    Cancelled,
    Expired,
}

impl DeadlineAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Completion {
    Success,
    Error,
    Cancelled,
    DeadlineExpired,
    Rejected,
    ConnectionClosed,
}

impl Completion {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::DeadlineExpired => "deadline_expired",
            Self::Rejected => "rejected",
            Self::ConnectionClosed => "connection_closed",
        }
    }
}

/// Stable structured tracing for one connection.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConnectionTrace {
    id: u64,
}

impl ConnectionTrace {
    pub(crate) fn new() -> Self {
        Self {
            id: NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub(crate) fn span(self) -> Span {
        info_span!("connection", connection_id = self.id)
    }

    pub(crate) fn request_span(self, method: &str, id: &RequestId) -> Span {
        let request_id = format_request_id(id);
        info_span!(
            "request",
            connection_id = self.id,
            direction = "inbound",
            kind = "request",
            method,
            request_id = %request_id,
            id = ?id,
        )
    }

    pub(crate) fn notification_span(self, method: &str) -> Span {
        info_span!(
            "notification",
            connection_id = self.id,
            direction = "inbound",
            kind = "notification",
            method,
        )
    }

    pub(crate) fn message(self, direction: Direction, message: &RawMessage) {
        let direction = direction.as_str();
        match message {
            RawMessage::Request { id, method, .. } => {
                let request_id = format_request_id(id);
                trace!(
                    connection_id = self.id,
                    direction,
                    kind = "request",
                    method = method.as_ref(),
                    request_id = %request_id,
                    "rpc message"
                );
            }
            RawMessage::Notification { method, .. } => trace!(
                connection_id = self.id,
                direction,
                kind = "notification",
                method = method.as_ref(),
                "rpc message"
            ),
            RawMessage::Response { id, .. } => {
                let request_id = format_request_id(id);
                trace!(
                    connection_id = self.id,
                    direction,
                    kind = "response",
                    request_id = %request_id,
                    "rpc message"
                );
            }
            RawMessage::ProtocolError { .. } => trace!(
                connection_id = self.id,
                direction,
                kind = "protocol_error",
                "rpc message"
            ),
        }
    }

    pub(crate) fn request_completed(
        self,
        method: &str,
        request_id: &RequestId,
        started: Instant,
        direction: Direction,
        completion: Completion,
    ) {
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let request_id = format_request_id(request_id);
        let direction = direction.as_str();
        let completion = completion.as_str();
        trace!(
            connection_id = self.id,
            direction,
            kind = "request",
            method,
            request_id = %request_id,
            latency_ms,
            completion,
            "request completed"
        );
    }

    pub(crate) fn resource_budget(
        self,
        resource: Resource,
        resource_action: ResourceAction,
        resource_current: usize,
        resource_limit: usize,
        bytes: Option<(usize, usize)>,
    ) {
        let resource = resource.as_str();
        let resource_action = resource_action.as_str();
        if let Some((resource_bytes, resource_bytes_limit)) = bytes {
            trace!(
                connection_id = self.id,
                resource,
                resource_action,
                resource_current,
                resource_limit,
                resource_bytes,
                resource_bytes_limit,
                "resource budget changed"
            );
        } else {
            trace!(
                connection_id = self.id,
                resource,
                resource_action,
                resource_current,
                resource_limit,
                "resource budget changed"
            );
        }
    }

    pub(crate) fn pending_request_budget(
        self,
        method: &'static str,
        resource_action: ResourceAction,
        request_id: u32,
        resource_current: usize,
        deadline: Option<Duration>,
    ) {
        let request_id = request_id.to_string();
        let resource_action = resource_action.as_str();
        let resource = Resource::PendingRequests.as_str();
        if let Some(deadline) = deadline {
            let deadline_ms = duration_ms(deadline);
            trace!(
                connection_id = self.id,
                direction = "outbound",
                kind = "request",
                method,
                request_id,
                resource,
                resource_action,
                resource_current,
                deadline_ms,
                "resource budget changed"
            );
        } else {
            trace!(
                connection_id = self.id,
                direction = "outbound",
                kind = "request",
                method,
                request_id,
                resource,
                resource_action,
                resource_current,
                "resource budget changed"
            );
        }
    }

    pub(crate) fn deadline(
        self,
        deadline: Deadline,
        deadline_action: DeadlineAction,
        direction: Direction,
        method: &str,
        request_id: &RequestId,
        limit: Duration,
        elapsed: Duration,
    ) {
        let request_id = format_request_id(request_id);
        let deadline = deadline.as_str();
        let deadline_action = deadline_action.as_str();
        let direction = direction.as_str();
        let deadline_ms = duration_ms(limit);
        let deadline_elapsed_ms = duration_ms(elapsed);
        trace!(
            connection_id = self.id,
            direction,
            kind = "request",
            method,
            request_id = %request_id,
            deadline,
            deadline_action,
            deadline_ms,
            deadline_elapsed_ms,
            "deadline changed"
        );
    }

    pub(crate) fn connection_closed(self, close_cause: &'static str) {
        trace!(connection_id = self.id, close_cause, "connection closed");
    }
}

fn format_request_id(id: &RequestId) -> Cow<'_, str> {
    match id {
        RequestId::Number(number) => Cow::Owned(number.to_string()),
        RequestId::String(string) => Cow::Borrowed(string),
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
