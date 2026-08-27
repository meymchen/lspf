# Stdio child connection owns process supervision

Status: Accepted.

A stdio-launched Client returns a `ChildConnection` that owns the generic `ClientConnection`, process handle, protocol driver, and stderr drain together, rather than teaching `Transport` about process lifecycle or making callers coordinate those resources. Its terminal operations reap the child after graceful LSP shutdown, a bounded exit wait, terminate, another bounded wait, and kill. Drop schedules graceful protocol cleanup on the caller's Tokio runtime and transfers process ownership to a reaper thread, whose synchronous terminate-kill-reap path survives runtime shutdown. Stderr is always drained and only its first 64 KiB is retained in `ChildOutput`, trading complete logs for a fixed memory bound and freedom from pipe deadlock.
