<script setup lang="ts">
import { computed } from 'vue';
import { useData } from 'vitepress';
import DefaultTheme from 'vitepress/theme';
import InfraScene from './components/InfraScene.vue';
import PointerGlow from './components/PointerGlow.vue';
const { lang } = useData();
const zh = computed(() => lang.value.toLowerCase().startsWith('zh'));
</script>

<template>
  <PointerGlow />
  <DefaultTheme.Layout>
    <template #home-hero-info-before>
      <p class="infra-eyebrow"><span aria-hidden="true" />RUST / LSP / IDE → AGENT</p>
    </template>
    <template #home-hero-image><InfraScene /></template>
    <template #home-hero-actions-after>
      <p class="infra-hero-note">{{ zh ? '类型化客户端与服务器 · 原生与 WASM' : 'Typed clients & servers · Native & WASM' }}</p>
    </template>
  </DefaultTheme.Layout>
</template>
