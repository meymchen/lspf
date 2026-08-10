# Execute-command capability preserves registration order

Status: Accepted. Supersedes the sorted, registration-order-independent
command-merging wording in
[ADR 0017](0017-typed-router-and-capability-catalog.md).

ADR 0017 stated that capability merging is "deterministic and independent of
registration order" and that "commands merge into one de-duplicated command
list," which the 0.2 implementation realized as a `BTreeSet`: the advertised
`executeCommandProvider.commands` came out sorted, unrelated to the order the
user registered their [[Command]]s in. Registration order is meaningful to
users — it is the order they declared their commands in, and the order editors
may surface them — and it is exactly as deterministic as a sort, because the
Router freezes one fixed registration sequence before capabilities are
computed.

The 0.3 decision is that the advertised execute-command list **exactly
matches the frozen Command registry**: de-duplicated, in registration order.
Static registrations come first, in builder-call order; conditional
registrations from the `configure_initialize` transaction follow, in the order
the registrar committed them. Duplicate names remain a `BuildError` (static)
or an initialize-transaction failure (conditional), so de-duplication in the
capability merge is a backstop, not the mechanism that resolves conflicts.

This supersedes only the command-list wording. Every other capability family
merge (completion plus resolve, rename plus prepare-rename, and the families
ADR 0017 enumerates) stays deterministic and independent of registration
order.

We rejected **keeping the sorted list**. Sorting is deterministic but
discards user intent, and no protocol requirement prefers alphabetical order;
the LSP specification places no ordering constraint on
`executeCommandProvider.commands`, so the framework is free to advertise the
order the user chose.

We rejected **registration-order-independent merging as a correctness
property** for commands. For commands there is nothing to merge: each name
appears once or the build fails, so "order independence" only ever described
the sort, not a conflict-resolution rule.
