//! Compile-time coverage for the frozen interface that exists only on WASM.
//!
//! CI builds this integration-test crate for `wasm32-unknown-unknown` with the
//! `worker-channel` feature. Imports are deliberately explicit: a removed or
//! renamed export fails at the same downstream boundary users compile against.

#![cfg(target_arch = "wasm32")]

#[allow(unused_imports)]
use lspf::{
    Client, ClientBuilder, ClientConnection, ClientContext, EmptyFileProvider, Outcome,
    ServerHandle, WorkerChannelBuilder, WorkerChannelReader, WorkerChannelTransport,
    WorkerChannelWriter, worker_channel,
};

#[test]
fn wasm_only_frozen_exports_are_nameable() {}
