import type { Theme } from 'vitepress';
import DefaultTheme from 'vitepress/theme';
import Layout from './Layout.vue';
import ArchitectureFlow from './components/ArchitectureFlow.vue';
import './custom.css';

export default {
  extends: DefaultTheme,
  Layout,
  enhanceApp({ app }) {
    app.component('ArchitectureFlow', ArchitectureFlow);
  },
} satisfies Theme;
