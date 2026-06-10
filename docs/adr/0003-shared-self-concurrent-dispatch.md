# `&self` handlers with concurrent dispatch

`LanguageServer` trait methods take `&self`. The dispatcher holds the
user's server in an `Arc` and spawns each incoming request or
notification as its own task, so multiple handlers can be in flight at
once. Users put their own mutable state behind interior mutability
(`Mutex`, `RwLock`, atomics) inside their server struct.

We rejected `&mut self`: it forces sequential dispatch, which makes one
slow handler (e.g. formatting) block every other one (hover, completion),
defeating the async runtime. We rejected actor-style channels: LSP
traffic is read-heavy across all of hover, completion, semantic-tokens,
and diagnostics, and a single-owner actor cannot exploit that
parallelism. We rejected `Arc<Self>` as the explicit receiver: same
concurrency as `&self` with worse signature ergonomics on every method.

The cost we accept: users hit Rust's interior-mutability rules whenever
they want to mutate their server's own state. We absorb the most common
case — the [[Document]] store — by shipping a concurrency-safe
[[Documents]] handle on the trait, so the typical handler reads
documents through `self.documents()` without writing any lock code.
