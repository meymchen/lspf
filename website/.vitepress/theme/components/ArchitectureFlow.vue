<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue';
import { useData } from 'vitepress';
import { protocolTraces, type TraceOwner } from './protocol-traces';

const { lang } = useData();
const zh = computed(() => lang.value.toLowerCase().startsWith('zh'));
const traces = computed(() => protocolTraces(zh.value));
const mode = ref(0);
const current = ref(0);
const expanded = ref(-1);
const visibleCount = ref(traces.value[0].events.length);
const playing = ref(false);
const reducedMotion = ref(false);
const viewport = ref<HTMLElement>();
const flow = computed(() => traces.value[mode.value]);
const visibleEvents = computed(() => flow.value.events.slice(0, visibleCount.value));
const owners = computed(() => ({
  wire: { label: zh.value ? '消息传输' : 'Message transport', detail: 'JSON-RPC', symbol: '↔' },
  protocol: { label: zh.value ? '框架控制' : 'Framework control', detail: zh.value ? '内部事件' : 'Internal event', symbol: '◇' },
  user: { label: zh.value ? '用户扩展点' : 'User extension', detail: zh.value ? '应用代码' : 'Application code', symbol: '{}' },
}));
const laneX: Record<TraceOwner, number> = { wire: 10, protocol: 26, user: 42 };
let timer: ReturnType<typeof setInterval> | undefined;
let preference: MediaQueryList | undefined;

function stop() {
  clearInterval(timer);
  timer = undefined;
  playing.value = false;
}
function selectMode(index: number) {
  stop();
  mode.value = index;
  expanded.value = -1;
  current.value = 0;
  visibleCount.value = flow.value.events.length;
  viewport.value?.scrollTo({ top: 0 });
}
async function revealCurrent() {
  await nextTick();
  const row = viewport.value?.querySelector<HTMLElement>('.trace-row.is-current');
  if (!viewport.value || !row) return;
  // Scroll only the message viewport; never move the document or keyboard focus.
  const offset = row.getBoundingClientRect().top - viewport.value.getBoundingClientRect().top;
  viewport.value.scrollTo({ top: viewport.value.scrollTop + offset - 12, behavior: 'auto' });
}
function selectEvent(index: number) {
  stop();
  current.value = index;
  expanded.value = expanded.value === index ? -1 : index;
}
function advance() {
  if (current.value >= flow.value.events.length - 1) { stop(); return; }
  expanded.value = -1;
  current.value += 1;
  visibleCount.value = Math.max(visibleCount.value, current.value + 1);
  void revealCurrent();
  if (current.value === flow.value.events.length - 1) stop();
}
function nextEvent() { stop(); advance(); }
function showAll() {
  stop();
  visibleCount.value = flow.value.events.length;
}
function play() {
  if (playing.value) { stop(); return; }
  if (reducedMotion.value) return;
  expanded.value = -1;
  if (visibleCount.value === flow.value.events.length) {
    current.value = 0;
    visibleCount.value = 1;
    void revealCurrent();
  }
  playing.value = true;
  timer = setInterval(advance, 1800);
}
function updatePreference() {
  reducedMotion.value = preference?.matches ?? false;
  if (reducedMotion.value) showAll();
}
function pauseWhenHidden() { if (document.hidden) stop(); }
onMounted(() => {
  preference = window.matchMedia('(prefers-reduced-motion: reduce)');
  updatePreference();
  preference.addEventListener('change', updatePreference);
  document.addEventListener('visibilitychange', pauseWhenHidden);
});
onBeforeUnmount(() => {
  stop();
  preference?.removeEventListener('change', updatePreference);
  document.removeEventListener('visibilitychange', pauseWhenHidden);
});
</script>

<template>
  <figure class="architecture-flow" :class="{ 'is-playing': playing }" :aria-label="zh ? 'LSP 协议消息瀑布图' : 'LSP protocol message waterfall'">
    <figcaption class="flow-heading">
      <span class="scene-label">LSP / PROTOCOL WATERFALL</span>
      <strong>{{ zh ? 'IDE 与语言服务的消息流' : 'The message flow behind your IDE' }}</strong>
      <span>{{ zh ? '示例消息与内部事件，按处理顺序展开。点击一行查看完整内容。' : 'Example messages and internal events in processing order. Select a row to inspect its contents.' }}</span>
    </figcaption>
    <div class="flow-tabs" role="group" :aria-label="zh ? '消息类型' : 'Message type'">
      <button v-for="(trace, index) in traces" :key="index" :aria-pressed="mode === index" @click="selectMode(index)">{{ trace.label }}</button>
    </div>
    <div class="trace-legend">
      <div v-for="(owner, key) in owners" :key="key" :data-owner="key">
        <span class="owner-symbol" aria-hidden="true">{{ owner.symbol }}</span>
        <span><strong>{{ owner.label }}</strong><small>{{ owner.detail }}</small></span>
      </div>
    </div>
    <p class="trace-context">{{ flow.context }}</p>
    <div class="trace-controls">
      <span class="trace-count" role="status" aria-live="polite">{{ zh ? '事件' : 'Event' }} {{ current + 1 }} / {{ flow.events.length }}</span>
      <div>
        <button v-if="!reducedMotion" class="flow-play" :aria-pressed="playing" @click="play"><span aria-hidden="true">{{ playing ? 'Ⅱ' : '▷' }}</span>{{ playing ? (zh ? '暂停' : 'Pause') : (zh ? '播放消息' : 'Play messages') }}</button>
        <button class="trace-next" :disabled="current === flow.events.length - 1" @click="nextEvent">{{ zh ? '下一条' : 'Next' }} <span aria-hidden="true">↓</span></button>
        <button class="trace-all" :disabled="visibleCount === flow.events.length" @click="showAll">{{ zh ? '显示全部' : 'Show all' }}</button>
      </div>
    </div>
    <div ref="viewport" class="waterfall-viewport" tabindex="0" role="region" :aria-label="zh ? '协议事件记录，可滚动' : 'Protocol event log, scrollable'">
      <ol class="trace-waterfall">
        <li v-for="(event, index) in visibleEvents" :key="`${mode}-${index}`" class="trace-row" :class="{ 'is-current': current === index }" :data-owner="event.owner">
          <div class="trace-rail" aria-hidden="true">
            <svg v-if="index" viewBox="0 0 52 40" preserveAspectRatio="none"><path :d="`M ${laneX[flow.events[index - 1].owner]} 0 V 12 L ${laneX[event.owner]} 28 V 40`" /></svg>
            <span class="rail-node" :style="{ left: `${laneX[event.owner]}px` }">{{ owners[event.owner].symbol }}</span>
          </div>
          <div class="trace-card">
            <button class="trace-event" :aria-expanded="expanded === index" @click="selectEvent(index)">
              <span class="trace-event-meta"><span class="owner-tag">{{ owners[event.owner].label }}</span><span class="trace-sequence">{{ String(index + 1).padStart(2, '0') }}</span></span>
              <strong>{{ event.title }}</strong>
              <span class="trace-route">{{ event.route }}</span>
            </button>
            <pre class="trace-payload" :class="{ 'is-expanded': expanded === index }"><code>{{ expanded === index ? event.payload : event.payload.replace(/\n\s*/g, ' ') }}</code></pre>
            <p v-if="expanded === index" class="trace-note">{{ event.note }}</p>
          </div>
        </li>
      </ol>
    </div>
    <p class="flow-note">{{ zh ? '↓ 顺序示意，不代表耗时或实时抓包。只有蓝色 ↔ 行是线上 JSON-RPC 消息；绿色 ◇ 为框架内部事件，紫色 { } 为应用扩展。传输分帧和错误分支已省略。' : '↓ Sequence only, not elapsed time or a live capture. Blue ↔ rows are wire JSON-RPC messages; green ◇ rows are framework internals; purple { } rows are application extensions. Transport framing and error branches are omitted.' }}</p>
  </figure>
</template>

<style scoped>
.architecture-flow {
  margin: 32px 0;
  padding: 24px;
  border: 1px solid var(--vp-c-divider);
  border-radius: 18px;
  background: linear-gradient(140deg, var(--vp-c-brand-soft), transparent 55%), var(--vp-c-bg);
  box-shadow: var(--infra-shadow);
}
.flow-heading { display: flex; flex-direction: column; gap: 8px; }
.flow-heading strong { font-size: 23px; line-height: 1.5; letter-spacing: -.025em; }
.flow-heading > span:last-child { font-size: 12px; color: var(--vp-c-text-2); }
.flow-tabs { display: flex; flex-wrap: wrap; gap: 6px; margin: 20px 0; }
.flow-tabs button { font-size: 13px; padding: 7px 12px; border: 1px solid var(--vp-c-divider); border-radius: 7px; color: var(--vp-c-text-2); }
.flow-tabs button[aria-pressed='true'] { color: var(--vp-c-brand-1); border-color: var(--vp-c-brand-1); background: var(--vp-c-brand-soft); }
[data-owner='wire'] { --owner-color: var(--infra-wire); --owner-line: solid; }
[data-owner='protocol'] { --owner-color: var(--infra-aqua); --owner-line: dashed; }
[data-owner='user'] { --owner-color: var(--infra-purple); --owner-line: double; }
.trace-legend { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px; }
.trace-legend > div { display: flex; align-items: center; gap: 10px; padding: 12px; border: 1px var(--owner-line) var(--owner-color); border-left: 4px solid var(--owner-color); border-radius: 8px; background: color-mix(in srgb, var(--owner-color) 9%, var(--vp-c-bg)); }
.owner-symbol { display: grid; place-items: center; flex-shrink: 0; width: 25px; height: 25px; font: 700 16px var(--vp-font-family-mono); white-space: nowrap; color: var(--owner-color); }
.trace-legend strong { display: block; font-size: 12px; color: var(--owner-color); }
.trace-legend small { display: block; font-size: 10px; color: var(--vp-c-text-2); }
.trace-context { margin: 16px 0 10px !important; font-size: 12px; color: var(--vp-c-text-2); line-height: 1.7; }
.trace-controls { display: flex; align-items: center; justify-content: space-between; gap: 10px; border-top: 1px solid var(--vp-c-divider); padding: 12px 0; }
.trace-controls > div { display: flex; flex-wrap: wrap; gap: 6px; }
.trace-count { flex-shrink: 0; font: 11px var(--vp-font-family-mono); color: var(--vp-c-text-2); }
.trace-controls button { padding: 5px 8px; font-size: 11px; border-radius: 5px; color: var(--vp-c-brand-1); }
.trace-controls button:hover:not(:disabled) { background: var(--vp-c-brand-soft); }
.trace-controls button:disabled { color: var(--vp-c-text-3); opacity: .65; cursor: default; }
.flow-play { border: 1px solid var(--vp-c-brand-1); }
.flow-play span { margin-right: 6px; }
.waterfall-viewport { max-height: 540px; overflow: auto; overscroll-behavior: contain; scrollbar-gutter: stable; scrollbar-width: thin; scrollbar-color: var(--vp-c-divider) transparent; padding: 4px 6px 4px 0; border-top: 1px solid var(--vp-c-divider); border-bottom: 1px solid var(--vp-c-divider); }
.trace-waterfall { list-style: none; margin: 0; padding: 0; }
.trace-row { display: grid; grid-template-columns: 54px minmax(0, 1fr); position: relative; margin: 0; padding-top: 12px; }
.trace-row:last-child { padding-bottom: 12px; }
.trace-rail { position: relative; background: linear-gradient(90deg, transparent 9px, color-mix(in srgb, var(--infra-wire) 20%, transparent) 9px 10px, transparent 10px 25px, color-mix(in srgb, var(--infra-aqua) 20%, transparent) 25px 26px, transparent 26px 41px, color-mix(in srgb, var(--infra-purple) 20%, transparent) 41px 42px, transparent 42px); }
.trace-rail svg { position: absolute; width: 52px; height: 40px; left: 0; top: -12px; overflow: visible; }
.trace-rail path { stroke: var(--owner-color); stroke-width: 1.5; fill: none; stroke-dasharray: 3 3; }
.rail-node { position: absolute; top: 28px; transform: translateX(-50%); width: 20px; height: 20px; display: grid; place-items: center; background: var(--vp-c-bg); color: var(--owner-color); font: 700 11px var(--vp-font-family-mono); white-space: nowrap; border: 1px var(--owner-line) var(--owner-color); border-radius: 4px; }
[data-owner='protocol'] .rail-node { border-radius: 50%; }
.trace-card { min-width: 0; border: 1px var(--owner-line) var(--owner-color); border-left: 4px solid var(--owner-color); border-radius: 8px; overflow: hidden; background: color-mix(in srgb, var(--owner-color) 5%, var(--vp-c-bg)); }
.trace-event { display: block; width: 100%; padding: 12px 15px 8px; text-align: left; }
.trace-event:hover { background: color-mix(in srgb, var(--owner-color) 8%, transparent); }
.trace-event-meta { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 6px; }
.owner-tag { font-size: 10px; color: var(--owner-color); font-weight: 600; }
.trace-sequence { font: 10px var(--vp-font-family-mono); color: var(--vp-c-text-2); }
.trace-event strong { display: block; font: 600 13px/1.6 var(--vp-font-family-mono); overflow-wrap: anywhere; }
.trace-route { display: block; font: 10px/1.8 var(--vp-font-family-mono); color: var(--vp-c-text-2); margin-top: 2px; }
.trace-payload { padding: 10px 14px; margin: 0 10px 10px; max-height: 7.5em; overflow: hidden; background: var(--vp-c-bg-alt); border: 1px solid var(--vp-c-divider); border-radius: 5px; font: 11px/1.7 var(--vp-font-family-mono); white-space: pre-wrap; overflow-wrap: anywhere; }
.trace-payload code { font: inherit; color: var(--vp-c-text-1); border: 0; background: none; padding: 0; }
.trace-payload.is-expanded { max-height: none; }
.trace-note { margin: 0 !important; padding: 0 14px 12px; color: var(--vp-c-text-2); font-size: 12px; line-height: 1.7; }
.is-current .trace-card { box-shadow: 0 0 0 2px color-mix(in srgb, var(--owner-color) 20%, transparent); }
.is-playing .trace-row:last-child .trace-card { animation: message-arrive .3s ease-out both; }
@keyframes message-arrive { from { opacity: .3; transform: translateY(-8px); } to { opacity: 1; transform: translateY(0); } }
.flow-note { margin: 14px 0 0 !important; font-size: 11px; line-height: 1.8; color: var(--vp-c-text-2); }
button:focus-visible, .waterfall-viewport:focus-visible { outline: 2px solid var(--vp-c-brand-1); outline-offset: 3px; }
.trace-event:focus-visible { outline-offset: -3px; }
@media (max-width: 639px) {
  .architecture-flow { padding: 18px 12px; }
  .flow-heading strong { font-size: 20px; }
  .flow-tabs button { font-size: 12px; padding: 6px 9px; }
  .trace-legend { gap: 5px; }
  .trace-legend > div { flex-direction: column; align-items: flex-start; gap: 3px; padding: 7px; border-left-width: 3px; }
  .trace-legend strong { font-size: 10px; }
  .trace-legend small { font-size: 9px; }
  .trace-controls { flex-wrap: wrap; gap: 6px; }
  .trace-controls button { padding: 5px 6px; font-size: 10px; }
  .trace-row { grid-template-columns: 47px minmax(0, 1fr); }
  .trace-event { padding: 10px 10px 7px; }
  .trace-event strong { font-size: 11px; }
  .trace-payload { padding: 8px; margin: 0 7px 8px; font-size: 10px; }
  .trace-note { padding-inline: 10px; font-size: 11px; }
  .waterfall-viewport { max-height: 520px; padding-right: 3px; }
}
@media (prefers-reduced-motion: reduce) {
  .is-playing .trace-row:last-child .trace-card { animation: none; }
}
</style>
