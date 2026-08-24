use std::time::Duration;

use lspf::{ResourcePolicy, ResourcePolicyField, Server};

fn build_error(policy: ResourcePolicy) -> lspf::BuildError {
    Server::builder(())
        .resource_policy(policy)
        .build()
        .err()
        .expect("an invalid resource policy must fail the build")
}

#[test]
fn production_resource_defaults_are_finite_and_buildable() {
    let policy = ResourcePolicy::default();

    assert_eq!(
        policy,
        ResourcePolicy {
            max_inbound_requests: 64,
            max_outbound_messages: 1_024,
            max_outbound_bytes: 16 * 1024 * 1024,
            max_documents: 1_024,
            max_document_bytes: 64 * 1024 * 1024,
            outbound_request_timeout: Some(Duration::from_secs(30)),
            handler_timeout: Duration::from_secs(30),
        }
    );

    Server::builder(())
        .resource_policy(policy)
        .build()
        .expect("the production resource policy builds");
}

#[test]
fn explicitly_disabled_outbound_request_deadline_is_buildable() {
    let policy = ResourcePolicy {
        outbound_request_timeout: None,
        ..ResourcePolicy::default()
    };

    Server::builder(())
        .resource_policy(policy)
        .build()
        .expect("an explicitly disabled outbound request deadline builds");
}

#[test]
fn zero_handler_deadline_fails_server_build() {
    let policy = ResourcePolicy {
        handler_timeout: Duration::ZERO,
        ..ResourcePolicy::default()
    };

    let error = Server::builder(())
        .resource_policy(policy)
        .build()
        .err()
        .expect("a zero handler deadline must fail the build");

    assert_eq!(
        error.to_string(),
        "resource policy `handler_timeout` must be greater than zero when enabled"
    );
}

#[test]
fn every_zero_connection_budget_fails_server_build() {
    let cases = [
        (
            ResourcePolicyField::MaxInboundRequests,
            ResourcePolicy {
                max_inbound_requests: 0,
                ..ResourcePolicy::default()
            },
        ),
        (
            ResourcePolicyField::MaxOutboundMessages,
            ResourcePolicy {
                max_outbound_messages: 0,
                ..ResourcePolicy::default()
            },
        ),
        (
            ResourcePolicyField::MaxOutboundBytes,
            ResourcePolicy {
                max_outbound_bytes: 0,
                ..ResourcePolicy::default()
            },
        ),
        (
            ResourcePolicyField::MaxDocuments,
            ResourcePolicy {
                max_documents: 0,
                ..ResourcePolicy::default()
            },
        ),
        (
            ResourcePolicyField::MaxDocumentBytes,
            ResourcePolicy {
                max_document_bytes: 0,
                ..ResourcePolicy::default()
            },
        ),
    ];

    for (field, policy) in cases {
        assert_eq!(
            build_error(policy),
            lspf::BuildError::InvalidResourcePolicy { field }
        );
    }
}

#[test]
fn zero_outbound_request_deadline_fails_server_build() {
    let policy = ResourcePolicy {
        outbound_request_timeout: Some(Duration::ZERO),
        ..ResourcePolicy::default()
    };

    assert_eq!(
        build_error(policy),
        lspf::BuildError::InvalidResourcePolicy {
            field: ResourcePolicyField::OutboundRequestTimeout
        }
    );
}
