---
title: 应用案例
description: 了解 lspf-analysis 如何使用 lspf 将代码健康诊断与悬停详情带入编辑器，并体验它的 VS Code 扩展。
---

<!-- markdownlint-disable-next-line MD025 -->
# 应用案例

## lspf-analysis

lspf-analysis 是使用 lspf 构建的代码健康语言服务器。它在编辑时分析函数与类，并在编辑器中提供诊断与悬停详情。它的 VS Code 扩展还提供 Code Health 视图，用于浏览分析结果。

<!-- markdownlint-disable-next-line MD033 -->
<p class="application-status">VS Code 扩展（预发布）</p>

<!-- markdownlint-disable-next-line MD033 -->
<div class="application-actions">

[在 VS Code 中体验](https://marketplace.visualstudio.com/items?itemName=meymchen.lspf-analysis)
[查看源码](https://github.com/meymchen/lspf-analysis)

<!-- markdownlint-disable-next-line MD033 -->
</div>

## 编辑时查看代码健康

分析覆盖 C++、Java、JavaScript、Python、Rust、TypeScript 和 TSX。在 VS Code 中，诊断显示在 Problems（问题）视图中，悬停可查看函数与类的详情，Code Health 视图列出函数指标，状态栏则提供当前文件的摘要。你可以对照正在修改的代码查看这些结果。

代码健康评分来自源码指标。[项目文档](https://github.com/meymchen/lspf-analysis#readme)说明了评分方式与配置选项。

## lspf 在其中的作用

应用通过 Rust 语言服务器把分析引擎与编辑器客户端连接起来。lspf 管理已同步文档与 LSP 连接，lspf-analysis 负责分析、评分和编辑器中的展示。

<!-- markdownlint-disable MD033 -->
<figure class="application-flow">
  <div class="application-flow-path">
    <div class="application-flow-node">
      <strong>编辑器客户端</strong>
      <span>文档变更与悬停请求</span>
      <span>诊断与 Code Health 视图</span>
    </div>
    <span class="application-flow-arrow" aria-hidden="true">↔</span>
    <div class="application-flow-server">
      <strong>lspf-analysis 服务器</strong>
      <div class="application-flow-layer">
        <strong>lspf</strong>
        <span>文档同步与 LSP 分发</span>
      </div>
      <div class="application-flow-layer">
        <strong>应用处理器与分析引擎</strong>
        <span>源码指标与代码健康评分</span>
      </div>
    </div>
  </div>
  <figcaption>编辑器变更通过 lspf 到达分析处理器，结果以诊断、悬停响应和应用自定义消息返回。</figcaption>
</figure>
<!-- markdownlint-enable MD033 -->

服务器使用文档生命周期通知触发分析。处理器读取已同步文档、发布诊断并响应悬停请求。自定义请求与通知则为编辑器界面传递文件摘要和函数详情。

这是一个拥有自身分析逻辑与客户端的下游应用。它的[服务器集成代码](https://github.com/meymchen/lspf-analysis/blob/main/crates/lspf-analysis/src/lib.rs)展示了 lspf 的协议功能如何在应用中配合使用。

## 在 VS Code 中体验

从 [Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=meymchen.lspf-analysis) 安装 LSPF Analysis。VS Code 扩展目前处于预发布阶段。安装与配置请参阅[扩展安装说明](https://github.com/meymchen/lspf-analysis/blob/main/clients/vscode/README.md)。

仓库还包含 IntelliJ IDEA 和 Visual Studio 客户端，其安装方式与可用状态由项目文档说明。这里链接的 Marketplace 扩展面向 VS Code。

## 阅读实现

从[服务器集成代码](https://github.com/meymchen/lspf-analysis/blob/main/crates/lspf-analysis/src/lib.rs)入手，查看处理器注册，以及分析结果如何接入 LSP 消息。[VS Code 客户端](https://github.com/meymchen/lspf-analysis/tree/main/clients/vscode)则展示了编辑器如何呈现这些结果。

应用使用的框架 API 可参考[功能注册](./guides/features-and-workspace)、[工作区状态](./guides/workspace-state)和[自定义消息](./guides/progress-and-custom-messages)。如果希望从更短的代码入手，可以阅读逐项演示协议功能的小型[功能示例服务器](./examples)。
