# Notebook sync joins the stable catalog

Status: Accepted. Supersedes the notebook exclusions in ADRs 0008 and 0017.
Its LSP 3.17 catalog boundary was later superseded by the complete stable LSP
3.18 boundary recorded in ADR 0024.

Notebook document synchronization was already stable in LSP 3.17. Excluding it
from the catalog left that catalog incomplete. Adding notebook synchronization
closed that consistency gap; the catalog now covers the complete stable LSP
3.18 surface, including notebook synchronization.

This record supersedes only ADR 0024's exclusion of notebook methods. ADR
0024's `on_initialized`, `on_exit`, and `on_shutdown` decisions remain in
force, as do its other catalog rules.

The framework stores notebook type, version, metadata, and ordered cell
membership in a notebook layer. It does not introduce a second text engine.
This is the same composition used by the TypeScript reference implementation.
Each [[Notebook cell]] is also an ordinary [[Document]] identified by its cell
URI, so its text lives in the existing rope-backed [[Documents]] store and
uses the existing full and incremental change path. This keeps position
encoding, URI identity, text mutation, and read access identical for notebook
cells and other text documents. A handler resolves notebook structure through
the notebook view and reads cell text through the existing documents view.

The connection's [[Resource policy]] meters notebook synchronization as one
resource model. Every cell counts toward the existing tracked-Document count,
and its text bytes count toward the existing Document-text byte budget. A
separate notebook-count limit bounds notebook-level metadata and permits an
empty notebook to consume finite capacity. Opening a notebook that would
exceed the notebook limit or either Document budget is refused through the
same resource-exhaustion path as an over-limit text-document open.

Notebook synchronization is opt-in, and advertising it is the opt-in.
`ServerBuilder::notebook_document_sync` both contributes the
`notebookDocumentSync` capability and makes the four notebook built-ins
reachable; a connection that advertised nothing ignores a notebook notification
that arrives anyway. This matches the reference TypeScript implementation, where
the server's notebook handlers exist only once the notebook sync feature is
applied and the client activates its side only when the server advertised the
capability. The rule also keeps notebook sync consistent with ADR 0023's
treatment of text-document sync, where an unadvertised notification is likewise
ignored rather than processed. Without it an unadvertised capability would still
mutate the notebook layer, open cell [[Document]]s against the connection's
[[Resource policy]], and reach a hook.

Notebook lifecycle notifications are protocol-owned built-ins. Their user
registrations are post-mutation hooks and observe the notebook layer and
[[Documents]] after the complete notebook mutation succeeds. Cell text changes
inside `notebookDocument/didChange` do not synthesize a
`textDocument/didChange` notification and therefore do not invoke its hook;
the notebook-change hook is the single observer for that wire notification.
The same rule applies when notebook open, structural change, or close adds or
removes cell Documents: those internal mutations do not invoke the
corresponding text-document hooks.

We rejected storing cell text in the notebook layer because it would duplicate
the rope and incremental-edit machinery and could give the same cell different
contents through the notebook and document views. We also rejected charging
only cell Documents: notebooks with no cells would then leave metadata and
ordering state outside the connection's finite resource policy.
