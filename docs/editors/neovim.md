# Neovim quick start

Run `lspf-markdown` with Neovim 0.11 or later. No plugin is required.
Install Rust 1.98 or later before building the server.

## Install and open a document

```bash
git clone https://github.com/meymchen/lspf.git
cd lspf
cargo install --path crates/lspf-markdown --locked
nvim --clean -u editor-validation/neovim/init.lua editor-validation/fixture/readme.md
```

Cargo's bin directory (`~/.cargo/bin` by default) must be on `PATH`.
If it is not, set `LSPF_MARKDOWN_SERVER` to the absolute installed binary path
before launching Neovim. On Windows, include the `.exe` suffix.

## Try the language features

The fixture contains a missing target and a link to `guide.md`.
Place the cursor **inside the path between parentheses**, then run:

| Cursor location | Command | Result |
| --- | --- | --- |
| `missing.md` | `:lua vim.diagnostic.open_float()` | A missing local target diagnostic from `lspf-markdown`. |
| `guide.md` | `:lua vim.lsp.buf.hover()` | The resolved target URI and its first heading. |
| `guide.md` | `:lua vim.lsp.buf.definition()` | Opens `guide.md` at its heading. |

Press `Ctrl-O` to return after the jump. Change `missing.md` to `guide.md` in
the first link; the diagnostic clears after the incremental edit.
The bundled config also provides `:LspfMarkdownRestart` and `:LspfMarkdownStop`.

## Connect your own server

Add this to `init.lua`, then open a Markdown file. This is an alternative to
the bundled config; use one configuration for this server.

```lua
vim.api.nvim_create_autocmd('FileType', {
  pattern = 'markdown',
  callback = function()
    vim.lsp.start({
      name = 'my-language-server',
      cmd = { 'my-language-server' },
      root_dir = vim.fs.root(0, '.git') or vim.fn.getcwd(),
    })
  end,
})
```

Replace the command with your executable or its absolute path. Set `pattern`
to your language's Neovim filetype. For the reference server, use
`cmd = { 'lspf-markdown' }`.

This follows Neovim's built-in
[`vim.lsp.start` configuration](https://neovim.io/doc/user/lsp.html#vim.lsp.start()).
The root becomes the server's workspace; choose your project's root marker
when `.git` does not describe it.

## If nothing happens

Run `:checkhealth vim.lsp` and `:lua print(vim.fn.executable('lspf-markdown'))`.
The latter should print `1` when using the command by name. Confirm the buffer
filetype with `:set filetype?`; it should be `markdown` for this fixture.
Use `:lua vim.lsp.log.open()` to inspect initialization or process errors.

Return to the [README](../../README.md), or follow the
[full editor validation journey](../../editor-validation/README.md).
