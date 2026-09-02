# URI identity keeps the fragment

Status: Accepted. Completes the `UriKey` rule fixed by
[ADR 0021](0021-workspace-snapshot-and-uri-identity.md), which
normalized the scheme, authority, path, and query but said nothing about the
fragment.

Notebook synchronization made the omission load-bearing. A [[Notebook cell]]'s
text-document URI must be unique across every cell of every notebook, and
clients mint that uniqueness in the fragment: the cells of one notebook arrive
as one path with differing fragments. A key that ignores the fragment merges
every cell of a notebook into a single [[Document]], so opening a notebook
tracks one cell instead of all of them and each cell's edits overwrite its
siblings — the same silent data loss ADR 0021 rejected raw-`Uri` keys to avoid,
arrived at from the other direction.

The fragment therefore joins the key, percent-decoded by the same rule as the
path and the query. Everything else ADR 0021 decided stands unchanged, and
public values still carry the client's original URI.

We rejected a notebook-only identity rule — cell URIs keyed one way and text
document URIs another. A cell *is* an ordinary Document (ADR 0034), read
through the same [[Documents]] view by the same URI, so two identity rules
would mean one URI resolving to two different documents depending on which
notification delivered it.

The cost we accept: two URIs that differ only in fragment are now two
Documents. RFC 3986 makes the fragment part of the URI reference, and a client
that opens `file:///a#L1` and `file:///a#L2` as separate text documents is
telling the server they are separate; the merged reading was never the intended
one.
