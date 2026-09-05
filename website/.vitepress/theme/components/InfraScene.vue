<script setup lang="ts">
import { computed, ref } from 'vue';
import { useData } from 'vitepress';
const { lang } = useData();
const zh = computed(() => lang.value.toLowerCase().startsWith('zh'));
const selected = ref(0);
const agentMode = ref(false);
const examples = computed(() => zh.value ? [
  { label: '读取符号', method: 'textDocument/hover', result: '类型化请求 → 语言服务器 → 类型化结果' },
  { label: '同步文档', method: 'textDocument/didChange', result: '文档变更通知 → 服务器更新文档状态' },
  { label: '取消请求', method: '$/cancelRequest', result: '请求 ID → 协议层 → 取消信号' },
] : [
  { label: 'Inspect a symbol', method: 'textDocument/hover', result: 'Typed request → language server → typed result' },
  { label: 'Sync a document', method: 'textDocument/didChange', result: 'Document notification → server updates document state' },
  { label: 'Cancel a request', method: '$/cancelRequest', result: 'Request ID → protocol layer → cancellation signal' },
]);
</script>

<template>
  <section class="infra-scene" :aria-label="zh ? 'IDE 基础与 Agent 扩展连接示意' : 'IDE foundation and agent extension connections'">
    <div class="scene-header"><span class="scene-label">LANGUAGE TOOLING</span><span class="scene-badge">{{ zh ? '连接示意' : 'CONNECTION MAP' }}</span></div>
    <div class="scene-modes" role="group" :aria-label="zh ? '连接场景' : 'Connection scenario'">
      <button :aria-pressed="!agentMode" @click="agentMode = false">{{ zh ? '01 · IDE 基础' : '01 · IDE foundation' }}</button>
      <button :aria-pressed="agentMode" @click="agentMode = true">{{ zh ? '02 · Agent 扩展' : '02 · Agent extension' }}</button>
    </div>
    <div class="scene-host"><span class="scene-glyph" aria-hidden="true">⌘</span><div><strong>{{ agentMode ? (zh ? '你的 Agent 宿主' : 'Your agent host') : (zh ? 'IDE / 编辑器' : 'IDE / Editor') }}</strong><span>{{ agentMode ? (zh ? '工具调用与应用策略' : 'Tool calls & application policy') : (zh ? '悬停、补全与诊断' : 'Hover, completion & diagnostics') }}</span></div><span class="scene-port" aria-hidden="true" /></div>
    <div class="scene-connector" aria-hidden="true"><span /></div>
    <div class="scene-core"><div class="scene-core-top"><img src="/logo-mark.svg" alt="" width="44" height="44" /><span>lspf<span class="scene-core-caption">{{ agentMode ? (zh ? '类型化 LSP 客户端' : 'Typed LSP client') : (zh ? '类型化语言服务器' : 'Typed language server') }}</span></span><span class="scene-core-tag">RUST</span></div><div class="scene-capabilities"><span>{{ agentMode ? (zh ? '请求关联' : 'Correlation') : (zh ? '文档同步' : 'Document sync') }}</span><span>{{ agentMode ? (zh ? '资源预算' : 'Budgets') : (zh ? '能力声明' : 'Capabilities') }}</span><span>{{ zh ? '取消机制' : 'Cancellation' }}</span></div></div>
    <div class="scene-connector" aria-hidden="true"><span /></div>
    <div class="scene-server"><span class="scene-glyph" aria-hidden="true">{ }</span><div><strong>{{ agentMode ? (zh ? '已有语言服务器' : 'Existing language server') : (zh ? '你的语言功能' : 'Your language features') }}</strong><span>{{ agentMode ? 'LSP / JSON-RPC · Transport' : (zh ? 'Handler · 应用状态 · 语言分析' : 'Handlers · application state · analysis') }}</span></div></div>
    <div class="scene-examples" role="group" :aria-label="zh ? '选择协议消息示例' : 'Choose a protocol message example'">
      <button v-for="(example, index) in examples" :key="index" :aria-pressed="selected === index" @click="selected = index">{{ example.label }}</button>
    </div>
    <div class="scene-message" aria-live="polite"><code>{{ examples[selected].method }}</code><p>{{ examples[selected].result }}</p></div>
    <p class="scene-footnote">{{ agentMode ? (zh ? '复用 LSP 语言能力，工具选择与模型调用仍由宿主负责。' : 'Reuse LSP language features; the host owns tool selection and model calls.') : (zh ? '从编辑器的语言体验出发，再扩展到 Agent 工具。' : 'Start with the editor’s language experience. Extend to agent tools.') }}</p>
  </section>
</template>
