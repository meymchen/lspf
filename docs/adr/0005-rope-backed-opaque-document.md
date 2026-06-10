# Rope-backed text storage, opaque to user code

The built-in [[Documents]] store keeps each [[Document]]'s text in a
`ropey::Rope`. The user never sees `ropey` — the `Document` API exposes
`uri()`, `language_id()`, `version()`, `text()`, `line(n)`,
`slice(range)`, and position/offset conversions, all returning
`Cow<'_, str>` where appropriate. The framework advertises incremental
text-document sync by default; users who want full sync flip the
`TEXT_DOCUMENT_SYNC` associated const on their `LanguageServer` impl.

We rejected a flat `String` backing because incremental edits on large
files (50k+ lines) become `O(n)` copies on every keystroke, which
conflicts with the "extreme performance" goal — particularly over a
WebSocket transport where every redundant byte is paid for in latency.
We rejected making the text store pluggable (a `Documents<T: TextStore>`
generic) because the generic parameter would bleed through
`LanguageServer` into every example and signature, and the rare user
who needs a different representation can override the doc-sync handlers
and stash their own state in their server struct.

The cost we accept: a non-trivial dependency on `ropey`, and the fact
that a user who genuinely needs CRDT-style or tree-sitter-tree storage
must walk away from the built-in store entirely. We treat that as the
correct trade — exotic needs pay the cost themselves; the common path
stays one type-parameter-free trait.
