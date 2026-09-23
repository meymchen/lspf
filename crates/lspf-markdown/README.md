# lspf-markdown

`lspf-markdown` is lspf's first-party Markdown language server, modeled on
[`vscode-markdown-languageservice`](https://github.com/microsoft/vscode-markdown-languageservice).
It targets CommonMark through [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark)
and provides:

- diagnostics for missing link targets and headings, undefined references, and
  duplicate or unused link definitions;
- hover, go to definition, and document links for link destinations, with
  image previews on hover;
- document and workspace symbols, folding ranges, and smart selection;
- find references, document highlights, and rename for headings, reference
  labels, and linked files;
- completion of link paths, heading fragments, and reference labels;
- code actions that organize link definitions, extract a link into one, and
  remove duplicate or unused definitions;
- link updates when the client renames files or directories.

Install the stdio server from this workspace:

```bash
cargo install --path crates/lspf-markdown
```

Configure an LSP client to launch `lspf-markdown` for the `markdown` language
ID. HTTP and other remote links are left to their owning clients; relative,
root-relative, and `file:` targets are resolved locally, and root-relative
targets resolve against the workspace folder that holds the document.
Cross-file features read every Markdown file in the workspace folders,
skipping dot-directories and `node_modules`.

Local links prefer an existing exact target. If an extensionless target is
absent, the server tries an existing `.md` file; other extensions must be
written explicitly. Missing targets retain their exact URI for diagnostics
and references, but cannot be renamed. File moves preserve link spelling when
it still selects the intended target, adding an explicit extension when needed.

The server speaks LSP over stdio. Run `lspf-markdown --listen <host:port>` to
serve one client over TCP instead, for example when a debugger owns the
process; the [VS Code quick start](../../docs/editors/vscode.md) uses this for
its client + server debugging entries.

To try it in an editor, follow the [Neovim quick start](../../docs/editors/neovim.md),
[VS Code quick start](../../docs/editors/vscode.md), or
[Zed quick start](../../docs/editors/zed.md). The
[30-second demo](../../docs/demo.md) uses the same fixture in Neovim.

The integration tests drive the real server through `lspf::testing`'s public
in-memory Transport seam:

```bash
cargo test -p lspf-markdown
```

The repository also contains repeatable VS Code, Neovim, and Zed validation
journeys in [`../../editor-validation`](../../editor-validation). Those
journeys install one packaged binary and keep protocol evidence separate from
human editor observations.
