<!--
Sync Impact Report
==================
Version change: (uninitialized template) → 1.0.0
Bump rationale: Initial ratification — replaces the bare placeholder template
  with the project's first complete governance document.

Structural change:
  - Discarded the generic 5-principle template structure.
  - Adopted the project-specific 7-section structure (§1–§7) defined by the
    maintainer in the /speckit-constitution invocation.

Sections in this version:
  - §1 Positioning (4 articles)
  - §2 Quality & Correctness (7 articles)
  - §3 API Stability & Evolution (6 articles)
  - §4 Dependencies & Runtime (5 articles)
  - §5 Scope / YAGNI (4 articles)
  - §6 Process & Governance (6 articles)
  - §7 Amendment Process (3 articles)

Added sections: all of the above (this is the initial ratification).
Removed sections: the placeholder "Core Principles" / SECTION_2 / SECTION_3 / Governance scaffolding from constitution-template.md.

Templates audit:
  ✅ .specify/templates/plan-template.md — already contains a generic
     "Constitution Check" gate section. It will be populated per-plan via §6.2,
     so no template edit is required at ratification time.
  ✅ .specify/templates/spec-template.md — no constitution-specific references;
     no changes required.
  ✅ .specify/templates/tasks-template.md — no constitution-specific references;
     no changes required.
  ✅ .specify/templates/checklist-template.md — generic; no changes required.
  ⚠ README.md, ROADMAP.md, RELEASING.md, CHANGELOG.md — referenced by §1.2,
     §5.1, §6.4, §6.5, §6.6 and will need to be authored as part of v0.1
     scaffolding work. Not blockers for ratification; tracked as follow-ups.

Deferred items / TODOs: none.
-->

# lspf Constitution

`lspf` is a Rust library crate that provides a higher-level abstraction layer over
existing LSP plumbing crates (tower-lsp, async-lsp, lsp-server). It targets the
gap in the Rust LSP ecosystem around closure-based handler registration,
automatic capability derivation, and built-in document management.

This constitution is **binding** on all contributors and reviewers. Articles use
imperative voice ("MUST", "MUST NOT", "SHOULD") and are numbered for citation
in PRs, plans, and reviews (e.g. "violates §3.2"). Every article is verifiable
via CI, PR review checklist, or AI cross-check during `/speckit-plan`.

## §1 Positioning

§1.1 The project MUST be a library crate providing an LSP **server framework**;
it MUST NOT be a concrete LSP server implementation, and it MUST NOT be an LSP
client. Within v0.x, no language-specific LSP implementation code may be merged
into the main workspace outside the `examples/` directory.

§1.2 The project's relationship to tower-lsp, async-lsp, and lsp-server MUST be
described as **complementary, not competitive**. PR descriptions and
documentation MUST NOT contain wording that claims to replace or supersede
those projects. The README MUST maintain a "How is this different" comparison
table covering at minimum tower-lsp, async-lsp, and lsp-server.

§1.3 The primary user-facing API style MUST be **closure registration**, not
large-trait implementation. Any PR that introduces a "user must implement a
trait with N methods" style API MUST explicitly mark itself as violating §1.3
and MUST be processed as a constitutional amendment per §7.

§1.4 The set of top-level publicly exposed concepts MUST NOT exceed 6:
`Server`, `ServerBuilder`, `Context<S>`, `Client`, `TextDocuments`,
`DocumentSnapshot`. Any PR adding a new top-level public type MUST justify in
its description why the new type cannot be expressed as a composition of the
existing 6 abstractions.

### 修订记录

- v1.0 — initial ratification

---

## §2 Quality & Correctness

§2.1 Every public API item (any `pub` item other than internal re-exports) MUST
be introduced **test-first**: a failing test MUST be committed before the
implementation. `/superpowers-test-driven-development` and `/execute-plan`
runs MUST follow red/green TDD. PR commit history MUST visibly show
test-before-impl ordering when only test and implementation commits are
involved (verifiable via `git log --oneline`).

§2.2 LSP protocol compliance: every implemented LSP method MUST have at least
one **contract test** that constructs a real `lsp-types` request structure,
runs it through the dispatcher, and asserts on the response structure.
Contract tests MUST live under `tests/lsp_contract/`.

§2.3 The "notifications serialized, requests concurrent" semantic MUST NOT be
broken. Any implementation that allows out-of-order notification processing
constitutes a correctness violation. An integration test named
`notification_ordering` MUST exist that concurrently submits multiple
`didChange` notifications plus one request and asserts that the request
observes state consistent with notifications applied in order.

§2.4 Position encoding MUST default to **UTF-16** (matching the LSP spec
default). `PositionEncoding` negotiation MAY be deferred to a later version,
but every offset-conversion function in the codebase MUST explicitly state its
encoding assumption in its doc comment, including the literal phrase
`"UTF-16 code units"`.

§2.5 Documentation completeness: the crate root `lib.rs` MUST enable
`#![warn(missing_docs)]` and `#![warn(rustdoc::broken_intra_doc_links)]`, and
CI MUST treat doc warnings as errors via `RUSTDOCFLAGS="-D warnings"`. Every
public item MUST carry at least a one-sentence summary; every public function
or struct MUST carry at least one `# Examples` block, with compilable doctests
preferred over fenced-off snippets.

§2.6 Cancellation MUST be cooperative. Each request handler receives a
`CancellationToken` via its `Context`, and the doc comment for that token MUST
state that "long-running computations should poll it actively". The framework
MUST fire the token upon receiving `$/cancelRequest`, but MUST NOT forcibly
interrupt the handler — the handler is contractually responsible for
honoring cancellation.

§2.7 Panic safety: a handler panic MUST be caught by the dispatcher and
converted to a JSON-RPC `InternalError` response; it MUST NOT terminate the
server process. An integration test named `handler_panic_isolation` MUST
exist and assert this behavior.

### 修订记录

- v1.0 — initial ratification

---

## §3 API Stability & Evolution

§3.1 All public structs and enums MUST be annotated `#[non_exhaustive]`,
unless the type is semantically closed (e.g. `enum Never {}`). Each exception
MUST be explicitly justified in the PR that introduces the type.

§3.2 The project MUST adhere strictly to SemVer. During the pre-1.0 phase,
breaking changes MUST be released via a minor bump (0.x.0), and CHANGELOG
entries describing them MUST be prefixed with `[BREAKING]`.

§3.3 The MSRV (Minimum Supported Rust Version) MUST be locked to the stable
release that was current six months prior to "today". The `rust-toolchain.toml`
file and the `rust-version` field in `Cargo.toml` MUST stay in sync. CI MUST
include a job that runs `cargo check` on the MSRV toolchain.

§3.4 Trait bounds on public APIs MUST be minimized. Any new bound on handler
closures beyond `Clone` and `'static` MUST be justified in the PR with a
demonstration that no alternative exists.

§3.5 The `lsp-types` dependency MUST track upstream stable releases via a `^x.y`
constraint in `Cargo.toml` (patch versions MUST NOT be pinned). A major-version
bump of `lsp-types` MUST be released as a minor bump of `lspf` and MUST be
recorded in the CHANGELOG with the `[BREAKING]` prefix, since it propagates to
downstream users.

§3.6 Public-API error types MUST implement
`std::error::Error + Send + Sync + 'static` and MUST provide a clear
`Display` implementation. Public APIs MUST NOT expose `anyhow::Error` or
`Box<dyn Error>` — error types MUST be project-defined named enums.

### 修订记录

- v1.0 — initial ratification

---

## §4 Dependencies & Runtime

§4.1 The default async runtime MUST be tokio. `runtime-agnostic` MAY exist as a
feature flag, but during v0.x its completeness MUST NOT be guaranteed and the
documentation MUST mark it as **experimental**.

§4.2 The crate's default Cargo features MUST be minimal: stdio transport +
tokio runtime + built-in document management only. TCP/socket transport,
`tracing` integration, and metrics MUST be opt-in feature flags.

§4.3 Any new public dependency (i.e. one appearing in a public API signature)
MUST be justified in the PR with: (a) why `std` is insufficient; (b) why
existing dependencies are insufficient; (c) the dependency's maintenance status
(at least one release in the last 12 months).

§4.4 Unsafe code MUST be forbidden by default. The crate root MUST contain
`#![forbid(unsafe_code)]`. Any unavoidable unsafe (e.g. FFI) MUST be isolated
in a dedicated companion crate that removes the `forbid` attribute at its own
crate root and accompanies every `unsafe` block with a `SAFETY:` comment.

§4.5 Async abstractions MUST be limited to `std::future::Future` plus tokio
primitives. Public APIs MUST NOT depend on additional async abstraction layers
(e.g. high-level combinators from the `futures` crate).

### 修订记录

- v1.0 — initial ratification

---

## §5 Scope / YAGNI

§5.1 The v0.1 scope is fixed at: closure-based handler registration; automatic
capability derivation; built-in `TextDocuments` with full sync; `Client`
reverse-communication handle; stdio transport; end-to-end support for the 10
most common LSP methods. Anything outside this scope MUST be entered into
`ROADMAP.md` under a v0.2+ milestone; "while we're at it" implementations are
prohibited.

§5.2 v0.1 MUST explicitly exclude: custom LSP extension method registration;
dynamic capability registration (`client/registerCapability`); progress /
work-done reporting; TCP transport; LSP-client functionality; procedural
macros; incremental document sync; multi-`workspaceFolder` negotiation.

§5.3 Any PR that introduces code outside the currently active spec's scope
MUST be closed or split. "Already wrote it, would be a waste to delete" is
NOT a valid merge justification.

§5.4 Example crates are scope-limited: v0.1 maintains exactly one full
example, `markdown-lsp`. Any additional example MUST first be discussed in an
issue and approved by a maintainer.

### 修订记录

- v1.0 — initial ratification

---

## §6 Process & Governance

§6.1 Every feature MUST follow the SDD pipeline: `/superpowers-brainstorming` →
`/speckit-specify` → `/speckit-plan` → `/speckit-tasks` → `/speckit-analyze` →
`/superpowers-executing-plans`. PRs that skip `/speckit-analyze` MUST NOT be
merged.

§6.2 Every `/speckit-plan` output MUST contain a section titled
"Constitution Compliance" that enumerates the constitutional articles touched
by the plan and, for each, states either "complies" or "requests an exception"
(with justification per §7).

§6.3 Commit messages MUST follow Conventional Commits using the prefixes
`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`, `ci:`. Breaking
changes MUST add a `BREAKING CHANGE:` line in the commit footer.

§6.4 Every PR MUST update the `[Unreleased]` section of `CHANGELOG.md`. CI
MUST include a job that verifies `[Unreleased]` is non-empty since the last
release, with the sole exception of PRs that only touch `docs`, `ci`, or
`test` files.

§6.5 The release procedure MUST be: bump version → rename CHANGELOG
`[Unreleased]` to `[x.y.z] - YYYY-MM-DD` → `cargo publish --dry-run` →
git tag → push tag → `cargo publish`. Each step MUST be documented in
`RELEASING.md`, and `RELEASING.md` MUST exist before the first release is
cut.

§6.6 `ROADMAP.md` MUST be maintained, grouped by v0.1 / v0.2 / v0.3 / 1.0
milestones, with each entry annotated as "已完成 / 进行中 / 计划中"
(done / in progress / planned). It MUST be re-synced at every release.

### 修订记录

- v1.0 — initial ratification

---

## §7 Amendment Process

§7.1 Constitutional amendments MUST go through a dedicated PR titled with the
prefix `constitution:`. The PR description MUST contain: (a) the article
numbers being amended; (b) the reason for the amendment; (c) the already-written
code and in-flight specs affected; (d) whether the amendment requires a
breaking-change release.

§7.2 Every amendment PR MUST update the "修订记录" subsection at the end of
each affected section, recording the new version (v1.0 → v1.1, etc.), the
date, and a one-line summary of the change.

§7.3 Amendments to §7 itself MUST clear a higher bar: a discussion period of
at least 7 days in a tracking issue, plus the explicit approval of a
maintainer.

### 修订记录

- v1.0 — initial ratification

---

**Version**: 1.0.0 | **Ratified**: 2026-05-10 | **Last Amended**: (none)

Ratified: v1.0 — 2026-05-10
Last amended: (none)

<!--
Self-check (per the meta rules in the /speckit-constitution invocation):

- [x] 每条条款是否都用祈使句？ — Every article uses MUST / MUST NOT / SHOULD /
      MAY (or 必须 / 禁止 / 应当 in the Chinese-origin phrasing kept verbatim
      where stylistically appropriate, e.g. §6.6 milestone status labels).
- [x] 每条条款是否都可验证？ — Each is verifiable by at least one of:
      CI job (§2.5, §3.3, §6.4), grep / static check (§2.4 doc comment phrase,
      §4.4 forbid attribute, §3.1 #[non_exhaustive], §3.6 error trait bounds),
      named integration test (§2.3, §2.7), PR review checklist (§1.2, §1.3,
      §1.4, §3.4, §4.3, §5.1, §5.3, §5.4, §6.1, §6.3), AI cross-check during
      /speckit-plan (§6.2 explicitly mandates the cross-check itself), or
      release-procedure document (§6.5, §6.6).
- [x] 是否有任何条款实质重复？ — No duplicates. §3.2 (SemVer) and §6.5
      (release procedure) are adjacent in topic but address distinct concerns
      (versioning rules vs. mechanical release steps). §1.3 (closure-style
      API) and §1.4 (six concepts cap) constrain different dimensions.
- [x] §5 范围条款是否与 §1 定位条款一致？ — §5.1 / §5.2 (in-scope /
      out-of-scope items) are consistent with §1.1 (framework not server),
      §1.3 (closure-style API → no proc-macro DSL needed), and §1.4 (six
      core types match the v0.1 surface).
- [x] §6 流程条款引用的命令名是否准确？ — Speckit slash commands use the
      `/speckit-<verb>` form (with hyphens, per the harness convention used
      in this project's .claude/skills/), and superpowers commands are cited
      as `/superpowers-brainstorming` and `/superpowers-executing-plans`,
      matching the skill names listed in the session's available skills.
- [x] 是否所有"禁止"条款都明确了例外路径？ — Every prohibition either
      embeds its exception inline (§3.1 "unless semantically closed", §4.4
      "isolated companion crate", §6.4 "docs/ci/test-only PRs", §1.1 v0.x
      scope, §4.1 runtime-agnostic flag) or routes through §7 amendment
      (§1.3, §1.4, §3.4, §4.3, §5.1, §5.3 implicitly).
- [x] 章节末尾的修订记录小节是否都已就位？ — §1 through §7 each carry a
      "修订记录" subsection seeded with "v1.0 — initial ratification".
-->
