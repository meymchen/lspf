# Protocol types come from a generated metaModel

Status: Accepted. Supersedes ADR 0014.

lspf will take its LSP protocol types from `gen-lsp-types`, generated from the
official LSP metaModel, and continue to re-export them under `lspf::types::*`.
This follows ADR 0014's instruction to re-evaluate the type base if
`lsp-types` stalled or the Rust LSP ecosystem moved. It does not mean ADR 0014
was wrong when it was accepted.

Both triggers have now fired. `lsp-types 0.97.0` was released on 2024-06-04
and remained its latest release when this decision was recorded on 2026-09-01,
nearly 27 months later. During that gap, `tower-lsp-server 0.23` moved from
`lsp-types` to the `ls-types` fork because it considered the original crate
unmaintained. That fork has since been archived in favour of
`gen-lsp-types`, which generates LSP 3.18 types from the official metaModel.
Meanwhile, the old base still lacks 3.18 types and some 3.17 surface that lspf
has had to patch by hand.

The dependency is pinned to exactly `gen-lsp-types = "=0.11.0"` with the
`fluent-uri` feature. The crate uses 0.x versioning because protocol-compatible
metaModel changes can still break its Rust API. The exact pin makes every
upgrade a deliberate, reviewable change with public-API and wire-shape checks;
it is not protecting downstream applications from churn, because lspf has no
downstream applications yet. A routine `cargo update` must not silently change
the protocol surface.

The migration spike in issue #237 confirmed that `fluent-uri` preserves the
URI boundary from ADR 0021. Public URI values retain their original spelling
and compare by that spelling, while the private `UriKey` remains authoritative
for normalized workspace identity. The default string representation was
rejected because it does not expose parsed URI components to `UriKey` or reject
invalid URIs. The `url` feature was rejected because WHATWG URL normalization
does not match the repository's RFC 3986 plus filesystem identity rules.

We rejected building an in-repository metaModel generator. The spike found no
blocker to direct adoption, so a generator would add an estimated 7–10 days of
generator, fixture, and CI work before paying the same repository migration
cost. It remains the escalation path if direct adoption later proves
unworkable. We also rejected staying on `lsp-types 0.97` and adding more local
patches: that would preserve known 3.18 gaps, extend the hand-maintained type
surface, and contradict the complete 3.18 boundary chosen for 1.0.

The spike prices direct adoption at 6–9 engineering days. Generated request
and notification markers fit the existing `FeatureSpec` design, but marker
names and method-key conversions change. Generated unions change construction
and matching across the public type surface. Explicit `null` versus absent
values affect the initialization and signature-help handler families, and
wire fixtures must be reviewed rather than mechanically accepted. The
migration also touches examples, guides, WASM checks, license checks, and
public-API approvals. These are adaptation costs, not reasons to keep a stalled
type base.
