---
title: 支持 LSP 的 Agent
description: 查找已提供 LSP 集成的编程 Agent，并直达它们的官方语言服务器配置指南。
---

语言服务器既能为 IDE 提供语言能力，也能为 Agent 提供代码上下文。通过下面的官方指南，可以配置现有 Agent 来启动语言服务器，包括你使用 lspf 构建的服务器。

## Agent 与配置指南

以下项目的官方文档明确说明了 LSP 集成和自定义语言服务器的配置方式。请按你使用的版本查阅对应指南。

| Agent | LSP 接入方式 | 官方配置指南 |
| --- | --- | --- |
| Claude Code | 通过插件注册语言服务器，提供诊断和代码导航。 | [LSP 服务器配置](https://code.claude.com/docs/en/plugins-reference#lsp-servers) · [安装代码智能插件](https://code.claude.com/docs/en/discover-plugins#code-intelligence) |
| OpenCode | 通过内置或自定义语言服务器，为 Agent 提供诊断反馈。 | [启用与配置 LSP](https://opencode.ai/docs/lsp/#configure) · [自定义 LSP 服务器](https://opencode.ai/docs/lsp/#custom-lsp-servers) |
| Crush | 通过语言服务器获取额外的代码上下文，支持在配置中注册自定义服务器。 | [LSP 接入说明](https://github.com/charmbracelet/crush#lsps) · [配置参考](https://github.com/charmbracelet/crush/blob/main/docs/config/README.md#lsp) |

### Claude Code

可以安装现有语言插件，也可以在插件的 `.lsp.json` 中注册自己的服务器。配置需要指定可执行程序，并通过 `extensionToLanguage` 将文件扩展名映射到语言标识符。服务器二进制需要单独安装，并加入 `PATH`。插件目录结构和可用选项见 [LSP 插件参考](https://code.claude.com/docs/en/plugins-reference#lsp-servers)。

Claude Code 的云端会话不会启动插件语言服务器，因此该环境中无法通过这种集成使用 LSP 工具。支持的运行环境见[代码智能指南](https://code.claude.com/docs/en/discover-plugins#code-intelligence)。

### OpenCode

当前官方文档要求在 `opencode.json` 中显式启用 LSP。注册自定义服务器时，在 `lsp` 对象中指定 `command` 数组和 `extensions`。请查阅[配置指南](https://opencode.ai/docs/lsp/#configure)，确认所用版本的启用默认值和支持的选项。

### Crush

当前官方文档使用 `crushrc` 中的 `lsp add` 注册服务器，可配置可执行程序、参数、文件类型和项目根目录标记。旧的 JSON 配置仍受支持，但已弃用。添加服务器时，请参阅 [LSP 配置参考](https://github.com/charmbracelet/crush/blob/main/docs/config/README.md#lsp)和[配置文件位置](https://github.com/charmbracelet/crush#configuration)。

## 接入使用 lspf 构建的服务器

按[服务器教程](../tutorials/server)构建原生 stdio 可执行程序，再根据上方对应 Agent 的指南注册它。如果想先试用一个具体的服务器，可在 lspf 仓库根目录安装 Markdown 参考服务器：

```bash
cargo install --path crates/lspf-markdown --locked
```

安装需要 Rust 1.98 或更新版本。在 Agent 的 LSP 配置中填入以下信息，字段名以该 Agent 的文档为准：

| 配置项 | Markdown 参考服务器 |
| --- | --- |
| 可执行程序 | `lspf-markdown`，需位于 Agent 进程的 `PATH` 中 |
| 启动参数 | 无，默认使用 stdio 传输 |
| 文件扩展名 | `.md` |
| LSP 语言标识符 | `markdown`，用于需要语言映射的 Agent |

使用包含失效本地链接的 Markdown 文件，检查 Agent 是否收到诊断。参考服务器还实现了悬停和定义跳转；这些操作是否以工具形式提供，取决于 Agent。具体行为和测试样例见[参考服务器](https://github.com/meymchen/lspf/tree/main/crates/lspf-markdown)。

资料核对日期为 2026 年 9 月 5 日。本页记录的是官方文档中的 LSP 支持情况，尚未针对这些 Agent 执行 lspf 集成测试。

## 构建自己的 Agent 集成

如果你负责 Agent 宿主，可以使用 lspf 的类型化 `Client` 连接语言服务器。[客户端教程](../tutorials/client)演示了打开文档、接收诊断、请求悬停和关闭服务器进程的完整流程。[客户端接入指南](client-adoption)介绍自定义传输与反向请求。如何将这些操作暴露为 Agent 工具，以及何时应用编辑，由宿主决定。
