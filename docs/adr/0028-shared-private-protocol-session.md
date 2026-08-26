# Shared private protocol session

Status: Accepted. Revises ADR 0018 and ADR 0019.

Server and Client endpoints share one crate-private `ProtocolSession` for JSON-RPC correlation, bounded admission and queues, handler deadlines, task ownership, writer coordination, and idempotent close. Endpoint engines retain their distinct lifecycle, registration, and domain-state policy; peer handles adapt their endpoint-specific registries to the shared close and writer operations. This seam avoids reimplementing concurrency invariants for the Client endpoint introduced after Issue #174 while keeping endpoint policy out of the shared core.
