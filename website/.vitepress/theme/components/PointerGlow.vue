<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue';

const glow = ref<HTMLElement>();
let preference: MediaQueryList | undefined;
let frame = 0;
let idleTimer: ReturnType<typeof setTimeout> | undefined;
let attached = false;
let visible = false;
let x = 0;
let y = 0;
let targetX = 0;
let targetY = 0;

function render() {
  x += (targetX - x) * 0.2;
  y += (targetY - y) * 0.2;
  glow.value?.style.setProperty('transform', `translate3d(${x}px, ${y}px, 0)`);
  frame = Math.abs(targetX - x) + Math.abs(targetY - y) > 0.2
    ? requestAnimationFrame(render) : 0;
}
function move(event: PointerEvent) {
  if (event.pointerType !== 'mouse') return;
  targetX = event.clientX;
  targetY = event.clientY;
  if (!visible) { x = targetX; y = targetY; }
  visible = true;
  glow.value?.setAttribute('data-visible', 'true');
  clearTimeout(idleTimer);
  idleTimer = setTimeout(hide, 120);
  if (!frame) frame = requestAnimationFrame(render);
}
function hide() {
  clearTimeout(idleTimer);
  idleTimer = undefined;
  visible = false;
  glow.value?.removeAttribute('data-visible');
  cancelAnimationFrame(frame);
  frame = 0;
}
function visibilityChanged() { if (document.hidden) hide(); }
function detach() {
  hide();
  document.removeEventListener('pointermove', move);
  document.documentElement.removeEventListener('pointerleave', hide);
  document.removeEventListener('visibilitychange', visibilityChanged);
  window.removeEventListener('blur', hide);
  attached = false;
}
function updatePreference() {
  if (!preference?.matches) { detach(); return; }
  if (attached) return;
  document.addEventListener('pointermove', move, { passive: true });
  document.documentElement.addEventListener('pointerleave', hide);
  document.addEventListener('visibilitychange', visibilityChanged);
  window.addEventListener('blur', hide);
  attached = true;
}
onMounted(() => {
  preference = window.matchMedia('(hover: hover) and (pointer: fine) and (prefers-reduced-motion: no-preference)');
  updatePreference();
  preference.addEventListener('change', updatePreference);
});
onBeforeUnmount(() => {
  detach();
  preference?.removeEventListener('change', updatePreference);
});
</script>

<template>
  <div class="pointer-glow-viewport" aria-hidden="true">
    <div ref="glow" class="pointer-glow" />
  </div>
</template>
