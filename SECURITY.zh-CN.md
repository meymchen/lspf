# 支持、兼容性与安全策略

[English](./SECURITY.md) | [简体中文](./SECURITY.zh-CN.md)

本文档是 `lspf` 已发布版本的支持契约，规定维护者支持的 Rust 版本、host、target 与
Cargo feature 组合，以及兼容性、弃用和安全报告规则。

## 版本支持窗口

`lspf` 不提供长期支持版本。维护范围仅包括最新 minor 版本线的最新 patch 版本。
例如，`0.6.0` 发布后，`0.5.x` 版本线就不再维护。维护者承诺修复 bug 前，用户必须能在
受维护版本线上复现该问题。

安全公告会列出所有已知受影响的版本，但修复以受维护版本线为目标。维护者可能向旧版本
移植风险较低的安全修复，但这不属于支持承诺。

## Rust 版本

最低支持 Rust 版本（MSRV）是 **1.96.0**。受支持的编译器范围从 Rust 1.96.0 到
最新 stable Rust 版本。nightly 与 beta toolchain 可用于提前发现问题，但不属于
受支持的 toolchain。

workspace 的 `rust-version` 字段是唯一准则。CI `feature-contract` 使用 Rust 1.96.0
为 Linux host 与 `wasm32-unknown-unknown` 编译文档中的每个 feature 组合。CI `msrv`
使用同一编译器检查面向发布的原生矩阵，并使用默认 feature 检查整个 workspace。CI
`native-matrix`、`test` 与 `wasm` 等其他 Rust job 使用 stable。

提高 MSRV 属于破坏性变更。1.0 之前，只能在新的 minor 版本中提高；1.0 之后，必须
发布新的 major 版本。release notes 必须写明新旧 MSRV。patch 版本不得提高 MSRV。

## 支持的 host 与 target

下表列出了全部受支持项。“受支持”表示维护者接受 bug 报告，并计划在受维护版本线上
修复 regression。

| Host | Rust target | 状态 | Enforcement gate |
| --- | --- | --- | --- |
| Linux | `x86_64-unknown-linux-gnu` | 受支持 | CI `native-matrix`、`test` 与 `native-lifecycle` |
| Windows | `x86_64-pc-windows-msvc` | 受支持 | CI `native-lifecycle` |
| macOS | `x86_64-apple-darwin` | 受支持 | CI `native-lifecycle` |
| macOS | `aarch64-apple-darwin` | 受支持 | CI `native-lifecycle` |
| Browser 或 Node Worker | `wasm32-unknown-unknown` | 受支持 | CI `wasm` 与 `feature-contract` |

CI `native-lifecycle` 会在 Linux、Windows 与 macOS 上运行相同的默认 feature stdio
journey。其他操作系统、架构、WASI target，以及嵌入式或 `no_std` 环境均不受支持。
维护者欢迎影响范围小且可移植的修复，但接受此类修复不会自动把对应 host 或 target
加入支持矩阵。

## Cargo feature 契约

默认 feature 只选择 `stdio`。下表涵盖全部受支持的 feature 组合。同一个受支持原生行
中的 feature 可以组合；`proposed` 可以添加到任意受支持行。其他跨 target 组合均不受
支持。

| Target 类别 | Feature 组合 | 状态 | Enforcement gate |
| --- | --- | --- | --- |
| 原生 | 默认 feature 或 `stdio` | 支持 stdio Transport | CI `msrv`、`native-matrix` 与 `test` |
| 原生 | `tcp` | 支持 TCP Transport | CI `msrv`、`native-matrix` 与 `test` |
| 原生 | `websocket` | 支持 WebSocket Transport | CI `msrv`、`native-matrix` 与 `test` |
| 原生 | `stdio`、`tcp` 与 `websocket` 的任意组合 | 受支持 | CI `msrv` 的 `all-native-features` 行与 CI `test` |
| 原生 | 只有 `runtime-tokio`，不启用第一方 adapter | 支持自定义 Transport | CI `feature-contract` |
| 原生 | 不启用 runtime 或 Transport feature | 支持仅协议编译，不支持运行 | CI `feature-contract` |
| 原生 | `worker-channel` | 明确无效 | CI `feature-contract` 检查编译期诊断 |
| `wasm32-unknown-unknown` | 只有 `wasm`，不启用第一方 adapter | 支持自定义 Transport | CI `wasm` |
| `wasm32-unknown-unknown` | `worker-channel` | 支持 MessagePort Transport；隐含 `wasm` | CI `wasm` 与 `feature-contract` |
| `wasm32-unknown-unknown` | 不启用 `wasm` | 明确无效 | CI `feature-contract` 检查编译期诊断 |
| `wasm32-unknown-unknown` | 默认 feature 或 `stdio` | 不受支持 | 不适用；该组合不属于支持契约 |
| `wasm32-unknown-unknown` | `tcp` 或 `websocket` | 明确无效 | CI `feature-contract` 检查编译期诊断 |
| 任意受支持行 | 添加 `proposed` | 作为不稳定 API surface 受支持；不会选择 runtime 或 Transport | CI `msrv` 的 `proposed` 行、CI `native-matrix` 与 `test` |

[Transport 指南](./docs/guides/transports.zh-CN.md)提供构建命令，并说明各 feature
启用的 API。CI `feature-contract` 会分别在启用与不启用 `proposed` 时编译每个受支持
组合，还会检查特定 Transport 的依赖不会泄漏到默认构建，并检查 `proposed` 与所有
Transport 相互独立。

## 语义化版本

发布版本遵循 Cargo 对 semantic versioning 的解释。公开 Rust API、Cargo feature
名称、默认 feature 集、已记录的行为和受支持 target 矩阵均属于兼容性契约。

crate 仍低于 1.0 时：

- patch 版本保持兼容；
- minor 版本可以包含破坏性变更，但 changelog 与 release notes 必须明确指出。

1.0 之后，破坏性变更必须发布新的 major 版本。增加可选 API 或放宽输入要求通常兼容。
改变默认 feature、删除 feature、收紧公开 bound 或移除受支持 target 均属于破坏性
变更。

`proposed` 后的 API 跟随 LSP specification 草案。patch 版本不会故意破坏这些 API，
但 minor 版本可以修改或删除它们，无须经过弃用周期。此类变更仍须写入 changelog。

CI `public API compatibility` 将 crate 与 crates.io 上的最新版本进行比较；该版本是当前
维护的 baseline。它会对表中每个 native feature selection、两个受支持的 WASM
selection，以及各 selection 加上 `proposed` 后的组合运行 `cargo-semver-checks`。默认
surface 与显式 `stdio` surface 相同，因此由该 row 覆盖。job 会上传 JSON artifact
`public-api-compatibility-report`，其中记录每个 surface 的 baseline、当前版本、命令输出、
结果与 exit code。

对于 WASM row，rustdoc 在 CI host 上运行，但会选择 crate 中
`target_arch = "wasm32"` 的代码分支。这规避了 `cargo-semver-checks` 的 target metadata
限制，同时仍会比较 WASM 专有 Rust interface。独立的 CI `wasm` 与 `feature-contract`
job 会为真实的 `wasm32-unknown-unknown` target 编译这些 selection。

gate 始终要求 `cargo-semver-checks` 按 patch 级兼容性执行检查。Exit code 100 表示工具
发现破坏性变更。报告中每个失败 row 都包含完整规范化 findings 的 hash。有意变更可在
`ci/public-api-breaking-approvals.json` 中按 baseline、当前版本、target 与 feature
selection 单独批准。出现不同或额外 finding 时，hash 会改变，gate 仍会失败。

若要批准已 review 的破坏性变更，请将失败 row 的 `baselineVersion`、
`currentVersion`、`target`、`features` 与 `findingsSha256` 值复制到 `approvals`
array 的新 entry 中。重新运行 gate，并将批准记录与破坏性变更一同 commit。对应的
`currentVersion` 发布后，应删除该批准记录；后续 baseline 或版本不能复用它。

批准记录仅适用于 1.0 前的 minor 版本增加或 1.0 后的 major 版本增加。Changelog 与
release notes 必须说明每个已批准变更。版本未变、patch 版本或 1.0 后的 minor 版本都不
接受批准记录。其他非零 tool exit 表示某个 surface 无法完成检查。Tool error 与未经
批准的 finding 都会在报告写入后使 CI job 失败。Setup failure 也会在退出前写入 JSON
error report。

## 弃用

稳定公开 API 必须先弃用，再删除。1.0 之前，它至少保留一个完整 minor 版本，并可在
下一个 minor 版本中删除。1.0 之后，它会保留到下一个 major 版本。弃用 notice 与
changelog 必须写明替代项，或者解释为何没有替代项。

如果 API 不健全、会导致漏洞或无法按文档工作，维护者可以跳过正常弃用窗口。release
notes 必须解释例外原因；存在安全替代方案时，还必须提供迁移说明。

公开 API 兼容性 gate 会检测已弃用与未弃用 API 的删除。CI `markdownlint` 检查本策略
和其他 Markdown 文件。CI `public docs` 对相同 target 与 feature surface 检查无
warning 的文档。

## 报告漏洞

请勿为疑似漏洞创建公开 issue。请使用 GitHub 的
[私密漏洞报告表单](https://github.com/meymchen/lspf/security/advisories/new)。报告应包含
受影响的版本与 feature、影响、复现步骤或 proof of concept，以及已知缓解措施。报告
不应包含真实用户数据或 credential。

维护者将：

1. 在三个工作日内确认收到报告；
2. 在七个日历日内提供初步严重程度与受影响版本评估；
3. 报告仍处于 open 状态时，至少每十四个日历日发送一次进展；
4. 与报告者协调修复和披露日期，然后发布 GitHub security advisory，写明受影响版本、
   缓解措施与已修复版本。

解决时间取决于严重程度与安全修复所需工作，因此没有固定解决期限。征得报告者同意后，
公告会为其署名。如果报告不属于漏洞，维护者会在私密报告中解释原因并关闭报告，也可能
建议为底层 bug 创建公开 issue。

本仓库已经启用 GitHub 私密漏洞报告。自动依赖 advisory 与 license gate 由 #160 跟踪；
在它落地前，依赖审查由人工完成。自动化不能替代针对 lspf 自有代码的私密漏洞报告。
