---
title: Agents with LSP support
description: Find coding agents with documented LSP integration and their official language-server configuration guides.
---

Language servers can provide code context to agents as well as IDEs. Use the
links below to configure an existing agent to launch a language server,
including a server you build with lspf.

## Agents and configuration guides

These projects document LSP integration and custom language-server
configuration. Follow the linked guide for the version you use.

| Agent | LSP integration | Official configuration guide |
| --- | --- | --- |
| Claude Code | Plugins register language servers for diagnostics and code navigation. | [LSP server configuration](https://code.claude.com/docs/en/plugins-reference#lsp-servers) · [Install code intelligence plugins](https://code.claude.com/docs/en/discover-plugins#code-intelligence) |
| OpenCode | Built-in and custom language servers supply diagnostics to the agent. | [Enable and configure LSP](https://opencode.ai/docs/lsp/#configure) · [Custom LSP servers](https://opencode.ai/docs/lsp/#custom-lsp-servers) |
| Crush | Language servers provide additional code context; custom servers can be registered in its configuration. | [LSP setup](https://github.com/charmbracelet/crush#lsps) · [Configuration reference](https://github.com/charmbracelet/crush/blob/main/docs/config/README.md#lsp) |

### Claude Code

Install an existing language plugin or register your own server in a plugin's
`.lsp.json`. The configuration specifies the executable and maps file
extensions to language identifiers with `extensionToLanguage`. Install the
server binary separately and make it available on `PATH`. See the
[LSP plugin reference](https://code.claude.com/docs/en/plugins-reference#lsp-servers)
for the plugin layout and available options.

Claude Code's cloud sessions do not start plugin language servers, so this
integration does not provide an LSP tool there. See the
[code intelligence guide](https://code.claude.com/docs/en/discover-plugins#code-intelligence)
for supported environments.

### OpenCode

The current documentation requires explicitly enabling LSP in `opencode.json`.
Use the `lsp` object to register a custom server with a `command` array and
`extensions`. Check the [configuration guide](https://opencode.ai/docs/lsp/#configure)
for enablement defaults and the options supported by your version.

### Crush

The current documentation uses `lsp add` inside `crushrc` to register a
server. Its options cover the executable, arguments, file types, and project
root markers. The older JSON configuration remains supported but is deprecated.
Use the [LSP configuration reference](https://github.com/charmbracelet/crush/blob/main/docs/config/README.md#lsp)
and [configuration locations](https://github.com/charmbracelet/crush#configuration)
when adding your server.

## Connect a server built with lspf

Build a native stdio executable with the [server tutorial](../tutorials/server),
then register that executable using your agent's guide above. For a concrete
server to try, install the Markdown reference server from an lspf checkout:

```bash
cargo install --path crates/lspf-markdown --locked
```

This requires Rust 1.98 or newer. Supply these values in the agent's LSP
configuration, using that agent's field names:

| Setting | Markdown reference server |
| --- | --- |
| Executable | `lspf-markdown`, installed on the agent process's `PATH` |
| Arguments | None; stdio is its default transport |
| File extension | `.md` |
| LSP language identifier | `markdown`, where the agent requires a mapping |

Use a Markdown file with a broken local link to check whether the agent
receives diagnostics. The reference server also implements hover and go to
definition; whether those operations are exposed as tools depends on the agent.
See the [reference server](https://github.com/meymchen/lspf/tree/main/crates/lspf-markdown)
for its behavior and fixtures.

Sources checked on September 5, 2026. This page records documented LSP support;
these agents have not been tested against lspf as part of this guide.

## Build your own agent integration

If you own the Agent host, use lspf's typed `Client` to connect to a language
server. The [Client tutorial](../tutorials/client) opens a document, receives
diagnostics, requests hover, and shuts down the server process. The
[client adoption guide](client-adoption) covers custom transports and reverse
requests. Your host decides how to expose those operations as agent tools and
when to apply edits.
