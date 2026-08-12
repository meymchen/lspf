# Configurable protocol-owned document synchronization

Status: Accepted. Extends ADR 0018's protocol built-ins and ADR 0017's
capability catalog.

Applications configure one effective `TextDocumentSyncOptions`, while the
protocol engine continues to own document-sync validation and mutation. The
effective options drive both advertised capabilities and inbound acceptance;
typed registrations infer unspecified save fields, and contradictory explicit
fields fail with `ConflictingCapability` instead of creating a route/capability
mismatch.

`textDocument/willSave` and `textDocument/didSave` are protocol-validated
notifications with post-validation hooks. They do not mutate `Documents`.
`textDocument/willSaveWaitUntil` remains a typed request feature whose sealed
descriptor contributes through `CapabilityBuilder`; the engine folds that
contribution into the protocol-owned sync capability.

We rejected ordinary Router notifications for save hooks because Layers could
then accept messages the server did not advertise, and rejected letting user
handlers mutate `Documents` because it would split document ownership between
the protocol engine and application code.
