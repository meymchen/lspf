# Workspace snapshot and normalized URI identity

Status: Accepted. Revises the `Workspace` scope fixed by
[ADR 0017](0017-typed-router-and-capability-catalog.md) — the Workspace is the
full initialization snapshot and owns the `Documents` handle — and extends the
initialize transaction in
[ADR 0018](0018-protocol-engine-and-outbound-request-broker.md).

`InitializeParams` carries more than workspace folders: client info, client
capabilities, initialization options, and the (deprecated) `rootUri`. Handlers
legitimately need all of it — a server cannot honor `initializationOptions`
it never stored. At the same time, one document arrives under many URI
spellings: VS Code sends `file:///c%3A/src/main.rs`, other clients send
`file:///C:/src/main.rs`, and `FILE` and `file` are one scheme. Keying the
[[Documents]] store by the raw `Uri` string would track the same file as two
documents and lose edits between them.

The [[Workspace]] established during the initialize transaction therefore
stores the **complete client-supplied snapshot** — client info, capabilities,
initialization options, root URI, and workspace folders, all verbatim, folder
order included — and owns the connection's [[Documents]] handle, handing out
only the read-only `DocumentsView`. [[Context]] exposes that established
Workspace directly: there is no workspace-less dispatch, so
`Context::workspace()` returns the Workspace itself and `Context::documents()`
is the same view `Workspace::documents()` hands out. `Workspace::roots()`
prefers the announced folders and falls back to one synthetic root derived
from `rootUri`, named for its final path segment — percent-decoded, since the
name is a display string — or `"workspace"` when there is none.

URI identity is one crate-private **`UriKey`**: scheme lowercased, the
authority's host lowercased (userinfo keeps its case — it is case-sensitive
per RFC 3986), path and query percent-decoded, and a leading Windows drive
letter in a `file:` path lowercased (`/C:` ≡ `/c:` ≡ `/c%3A`). Ordinary path
case is preserved — `file:///Foo` and `file:///foo` stay distinct — because
only the drive letter carries Windows case-insensitive semantics; the
filesystem beyond it may be case-sensitive. Public values (`Document::uri()`,
`WorkspaceFolder`, `Workspace::root_uri()`) always keep the client's original
URI; normalization exists only inside the key.

We rejected **raw-`Uri` keys** (the pre-ADR behavior). Equivalent spellings
of one document then occupy separate store entries, and a `didChange`
spelled differently from its `didOpen` edits a different document — silent
data loss, not a cosmetic mismatch.

We rejected **RFC 3986 unreserved-only percent-decoding**. It is the
standard's own normalization rule, but it leaves reserved characters encoded,
so `c%3A` never equals `c:` — the single most common spelling divergence a
Windows client produces. Full percent-decoding matches what converting a
`file:` URI to a filesystem path does, which is the identity a language
server actually cares about.

We rejected **lowercasing the whole path** (VS Code's `URI` comparison does
this for its own caches). It merges files that differ only in case on
case-sensitive filesystems — wrong for a server that must address the file
the client means.

We rejected **keeping `Option<Workspace>` in [[Context]]**. The engine
rejects every request and drops every notification before the initialize
transaction establishes the Workspace, so the `None` case was unobservable
by user code; making the Workspace non-optional removes a unwrap-or-ignore
ceremony from every handler.

The cost we accept: `UriKey` is framework-internal, so user code comparing
URIs on its own reimplements the rule — acceptable until a later slice shows
that need. Full percent-decoding also treats `a%2Fb` and `a/b` as one
document; for a filesystem path those decode to the same file, so the merge
is correct where it matters.
