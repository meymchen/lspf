# Document text queries compose through ranges

The Document read surface uses one `text(encoding, Option<Range>)` method for
whole and selected text, with `line_count` and `line_range` for line queries.
This revises the separate line/slice sketch in [ADR 0005](0005-rope-backed-opaque-document.md)
and replaces the published `text() -> String` signature. It preserves opaque
storage, immutable snapshots, and the explicit position encoding described in
[ADR 0016](0016-position-encoding.md).

Returning a line range lets handlers use it directly in a response or pass it
to the same text reader. A `TextLine` type or an encoding-bound view would add
another public contract before callers need it. Word lookup still returns
both text and range because rename handlers consume them together; character
membership remains an application-supplied predicate.

The trade-off is that whole reads also take an encoding, which is ignored,
and return an `Option` that is always `Some`. `None` selects the full snapshot;
invalid explicit ranges return `None` instead of being clamped. Callers must
chain fallible range lookups with `and_then`, and use `Cow::into_owned` when
text must outlive the snapshot. This contract keeps Unicode boundary checks
and partial materialization in Document without adding another selection type.

The existing Unicode line model and public position/offset conversion behavior
remain unchanged. Aligning those legacy contracts with LSP line-ending and
out-of-range position rules is a separate decision.
