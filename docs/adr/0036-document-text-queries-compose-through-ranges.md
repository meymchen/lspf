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

`text` returns `Cow<str>` directly. `None` or an invalid explicit range selects
the full snapshot; a valid empty range selects an empty string. This makes an
invalid range behave like an omitted selection, without clamping endpoints or
returning a partially valid fragment. Line-range and word queries still return
`None` when absent. Callers compose a fallible line lookup with `map` and use
`Cow::into_owned` when text must outlive the snapshot. Valid partial reads only
materialize their selected text; a full-document fallback can materialize the
whole snapshot. Unicode boundary checks remain inside Document.

The maintainer approved a one-time versioning exception: retain this API and
release it as `1.0.3`, despite its incompatibility with `1.0.2`. The release PR
must state the break and migration. After release-plz generates that PR,
`ci/prepare-release-pr.sh` uses release-plz's workspace-aware `set-version` to
adjust it. This applies only while the base workspace version is `1.0.2`;
after the `1.0.3` release PR merges, ordinary version selection resumes.
Compatibility checks and their exact finding approvals remain enabled.
