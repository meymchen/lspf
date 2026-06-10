# Server capabilities are auto-derived from the trait impl

The `LanguageServer` trait carries one associated const per LSP feature
(`const HOVER: bool = false;`,
`const COMPLETION: Option<CompletionConfig> = None;`, …). The default
`server_capabilities()` method reads those consts and builds the
`ServerCapabilities` returned from `initialize`. Users who opt in to a
feature flip the const and implement the matching handler; users who
need exotic capability wiring override `server_capabilities()` directly.

We rejected forcing users to hand-write `ServerCapabilities`: it's
boilerplate, it's the most common source of "my handler isn't running"
bugs in LSP frameworks, and it works against the "minimal user code"
goal. We rejected proc-macro derivation (which could inspect the impl
block and infer capabilities from which methods are actually overridden)
because ADR 0002 keeps the framework macro-free for contributor
ergonomics and compile-time cost.

The cost we accept: a user can lie — set `HOVER = true` without
implementing `hover`, or implement `hover` while leaving `HOVER = false`.
Neither crashes; the client either calls a default no-op or never calls
at all. We treat this as a documentation and example-quality problem,
not a soundness one.
