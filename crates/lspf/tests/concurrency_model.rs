//! Deterministic concurrency models for the protocol-session invariants.

mod concurrency_model_support;

use concurrency_model_support::{Scenario, diagnostic_failure, explore};

#[test]
fn response_and_cancellation_complete_each_request_exactly_once() {
    explore(Scenario::response_versus_cancellation());
}

#[test]
fn out_of_order_responses_preserve_request_correlation() {
    explore(Scenario::out_of_order_outbound_responses());
}

#[test]
fn bounded_queue_never_exceeds_limits_and_releases_every_charge() {
    explore(Scenario::bounded_queue_versus_writer_and_close());
}

#[test]
fn concurrent_close_is_idempotent_and_joins_owned_tasks() {
    explore(Scenario::task_and_request_versus_repeated_close());
}

#[test]
fn writer_failure_wins_before_quiescence_and_releases_the_queue() {
    explore(Scenario::writer_failure_versus_eof());
}

#[test]
fn cancellation_and_completion_release_inbound_capacity() {
    explore(Scenario::capacity_reuse_after_completion());
}

#[test]
fn failing_schedule_reports_an_exact_replay_trace() {
    let failure = diagnostic_failure();

    assert!(failure.contains("concurrency model `diagnostic-missing-completion` failed"));
    assert!(failure.contains("replay trace:\nwriter:Send"));
}
