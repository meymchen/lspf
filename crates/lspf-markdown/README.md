# lspf-markdown

`lspf-markdown` is lspf's first-party Markdown link language server. It uses
incremental document synchronization to report missing local link targets,
shows the resolved URI and first target heading on hover, and navigates a link
definition to that heading.

Install the stdio server from this workspace:

```bash
cargo install --path crates/lspf-markdown
```

Configure an LSP client to launch `lspf-markdown` for the `markdown` language
ID. HTTP and other remote links are left to their owning clients; relative,
root-relative, and `file:` targets are resolved locally.

The integration tests drive the real server through `lspf::testing`'s public
in-memory Transport seam:

```bash
cargo test -p lspf-markdown
```
