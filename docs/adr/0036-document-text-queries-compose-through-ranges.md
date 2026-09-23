# Document text queries compose through ranges

The Document read surface uses one `text(Option<Range>)` method for whole and
selected text, with `line_count` and `line_range` for line queries. Returning a
line range lets handlers use it directly in a response or pass it to the text
reader. Word lookup returns both text and range because rename handlers consume
them together; character membership remains an application-supplied predicate.
This revises the separate line/slice sketch in [ADR 0005](0005-rope-backed-opaque-document.md)
and replaces the published `text() -> String` signature.

Each Document retains the connection's negotiated `PositionEncoding` alongside
its text and metadata. Open documents, provider-loaded snapshots, and Notebook
cells capture that value when created; clones retain it across later changes
and closes. All position-based reads and conversions use the retained value,
and `position_encoding()` exposes it for application calculations. This revises
[ADR 0016](0016-position-encoding.md): Document conversion methods no longer take
an explicit encoding. Carrying the value in the snapshot keeps text and its
coordinate units together without another view type or live context lookup.
The existing Unicode line model and conversion behavior remain unchanged.

`text` returns `Cow<str>` directly: `None` selects the full snapshot, and empty
or invalid explicit ranges return an empty string. Invalid endpoints are never
clamped, and reversed ranges do not select text. This deliberately makes an
invalid selection indistinguishable from a valid empty one, so callers can
consume text without handling an optional result. Line-range and word queries
still return `None` when absent. Callers compose a fallible line lookup with
`map` and use `Cow::into_owned` when text must outlive the snapshot. Unicode
boundary checks and partial materialization remain inside Document.
