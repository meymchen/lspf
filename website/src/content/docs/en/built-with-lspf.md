---
title: Built with lspf
description: See how lspf-analysis uses lspf to bring code health diagnostics and hover details into the editor, and try its VS Code extension.
---

<!-- markdownlint-disable-next-line MD025 -->
# Built with lspf

## lspf-analysis

lspf-analysis is a code health language server built with lspf. It analyzes
functions and classes as you edit, with diagnostics and hover details in the editor.
Its VS Code extension also provides a Code Health view for exploring analysis results.

<!-- markdownlint-disable-next-line MD033 -->
<p class="application-status">VS Code extension · Pre-release</p>

<!-- markdownlint-disable-next-line MD033 -->
<div class="application-actions">

[Try in VS Code](https://marketplace.visualstudio.com/items?itemName=meymchen.lspf-analysis)
[Source code](https://github.com/meymchen/lspf-analysis)

<!-- markdownlint-disable-next-line MD033 -->
</div>

## Code health while you edit

The analysis covers C++, Java, JavaScript, Python, Rust, TypeScript, and TSX.
In VS Code, diagnostics appear in the Problems view, hover shows function and class
details, and the Code Health view lists function metrics. The status bar summarizes
the active file. You can inspect the results alongside the code you are changing.

Code health scores come from source-code metrics. The
[project documentation](https://github.com/meymchen/lspf-analysis#readme)
explains the scoring and configuration options.

## How lspf fits

The application combines an analysis engine with editor clients through a Rust
language server. lspf manages the synchronized documents and LSP connection;
lspf-analysis owns the analysis, scoring, and editor presentation.

<!-- markdownlint-disable MD033 -->
<figure class="application-flow">
  <div class="application-flow-path">
    <div class="application-flow-node">
      <strong>Editor client</strong>
      <span>Document changes and hover requests</span>
      <span>Diagnostics and Code Health view</span>
    </div>
    <span class="application-flow-arrow" aria-hidden="true">↔</span>
    <div class="application-flow-server">
      <strong>lspf-analysis server</strong>
      <div class="application-flow-layer">
        <strong>lspf</strong>
        <span>Document synchronization · LSP dispatch</span>
      </div>
      <div class="application-flow-layer">
        <strong>Application handlers and analysis engine</strong>
        <span>Source-code metrics · Code health scores</span>
      </div>
    </div>
  </div>
  <figcaption>Editor changes reach the analysis handlers through lspf. Results return as diagnostics, hover responses, and application-specific messages.</figcaption>
</figure>
<!-- markdownlint-enable MD033 -->

The server uses document lifecycle notifications to trigger analysis. Its handlers
read synchronized documents, publish diagnostics, and answer hover requests.
Custom requests and notifications carry file summaries and function details for
the editor UI.

This is a downstream application with its own analysis and clients. Its
[server integration](https://github.com/meymchen/lspf-analysis/blob/main/crates/lspf-analysis/src/lib.rs)
shows how lspf's protocol features work together in an application.

## Try it in VS Code

Install LSPF Analysis from the
[Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=meymchen.lspf-analysis).
The VS Code extension is currently a pre-release. Follow the
[extension's installation instructions](https://github.com/meymchen/lspf-analysis/blob/main/clients/vscode/README.md)
for setup and configuration.

The repository also contains IntelliJ IDEA and Visual Studio clients. Their setup
and availability are documented in the project; the Marketplace extension
linked here is for VS Code.

## Read the implementation

Start with the
[server integration](https://github.com/meymchen/lspf-analysis/blob/main/crates/lspf-analysis/src/lib.rs)
to see handler registration and the connection between analysis and LSP messages.
The [VS Code client](https://github.com/meymchen/lspf-analysis/tree/main/clients/vscode)
shows how the editor presents those results.

For the framework APIs behind the application, see
[feature registration](./guides/features-and-workspace),
[workspace state](./guides/workspace-state), and
[custom messages](./guides/progress-and-custom-messages).
The small [feature example servers](./examples) isolate individual protocol features
when you want a shorter starting point.
