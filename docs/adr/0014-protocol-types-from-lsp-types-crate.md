# Protocol types come from the `lsp-types` crate, re-exported by lspf

lspf depends on the `lsp-types` crate (gluon-lang/lsp-types) for every LSP
wire type: `InitializeParams`, `ServerCapabilities`, `HoverOptions`,
`Diagnostic`, and the long tail. Those types are re-exported under
`lspf::types::*` so users add `lspf` to their `Cargo.toml` and never list
`lsp-types` directly. The `LanguageServer` trait's capability associated
consts (ADR 0004) carry `lsp-types` types as-is
(`const COMPLETION: Option<lsp_types::CompletionOptions> = None;`); the
outgoing helpers on [[Context]] (ADR 0009) take and return `lsp-types`
structs.

We considered building on Microsoft's `lsprotocol` crate (generated from
the LSP metamodel) and writing our own types from scratch. We rejected
both. `lsprotocol`'s Rust target is at `1.0.0-alpha.3` with ~32 lifetime
downloads, last released mid-2025, and has no foothold in the Rust LSP
ecosystem; `lsp-types` is at 0.97, has 6M+ downloads, underlies
rust-analyzer, tower-lsp, async-lsp, and helix's LSP client, and its
repo is actively maintained. The original `lsprotocol` pitch
("auto-generated, always tracks the spec") has no deficit to close: LSP
3.17 has been the current spec since 2022, and `lsp-types` keeps pace on
the corners that matter. Rolling our own types costs ~3000 lines of
boilerplate maintained in lockstep with the spec for zero user benefit;
users who already know LSP recognize `lsp_types::Hover`, not
`lspf::Hover`.

The cost we accept: `lsp-types` is now part of lspf's public-API
contract. A SemVer-major bump of `lsp-types` is a SemVer-major bump of
lspf, and any opinionated choice in its surface (e.g. the `OneOf<A, B>`
wrappers it uses for "A or B" wire fields) leaks into ours. We
re-evaluate if the Rust LSP ecosystem migrates to `lsprotocol` later —
most likely after Microsoft invests in it or `lsp-types` stalls — and
until then, going with the crate every adjacent project uses is the
lower-risk move. Spec corners `lsp-types` does not yet model are handled
with `serde_json::Value` fields, not by switching crates.
