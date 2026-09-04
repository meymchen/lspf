import { defineConfig } from 'astro/config';
import { unified } from '@astrojs/markdown-remark';
import starlight from '@astrojs/starlight';
import remarkRustdocLinks from './src/remark/rustdoc-links.mjs';

const repository = 'https://github.com/meymchen/lspf';

export default defineConfig({
  site: 'https://meymchen.github.io',
  base: '/lspf',
  trailingSlash: 'always',
  markdown: {
    processor: unified({ remarkPlugins: [remarkRustdocLinks] }),
  },
  integrations: [
    starlight({
      title: 'lspf',
      description: 'A Rust framework for building extensible language servers.',
      logo: {
        light: './src/assets/logo.svg',
        dark: './src/assets/logo-dark.svg',
        alt: 'lspf',
        replacesTitle: true,
      },
      favicon: '/favicon.svg',
      defaultLocale: 'en',
      locales: {
        en: { label: 'English', lang: 'en' },
        'zh-cn': { label: '简体中文', lang: 'zh-CN' },
      },
      social: [
        { icon: 'github', label: 'GitHub', href: repository },
      ],
      editLink: {
        baseUrl: `${repository}/edit/main/website/`,
      },
      lastUpdated: true,
      customCss: ['./src/styles/custom.css'],
      head: [
        { tag: 'meta', attrs: { name: 'theme-color', content: '#0b0f14' } },
        { tag: 'meta', attrs: { property: 'og:type', content: 'website' } },
        { tag: 'meta', attrs: { property: 'og:site_name', content: 'lspf documentation' } },
      ],
      sidebar: [
        {
          label: 'Learn',
          translations: { 'zh-CN': '学习' },
          items: [
            { label: 'Introduction', translations: { 'zh-CN': '简介' }, slug: 'getting-started' },
            { label: 'Core concepts', translations: { 'zh-CN': '核心概念' }, slug: 'concepts' },
          ],
        },
        {
          label: 'Tutorials',
          translations: { 'zh-CN': '教程' },
          items: [
            { label: 'Build a language server', translations: { 'zh-CN': '构建语言服务器' }, slug: 'tutorials/server' },
            { label: 'Drive a language server', translations: { 'zh-CN': '驱动语言服务器' }, slug: 'tutorials/client' },
          ],
        },
        {
          label: 'Build a server',
          translations: { 'zh-CN': '构建服务器' },
          items: [
            { label: 'Register features', translations: { 'zh-CN': '注册功能' }, slug: 'guides/features-and-workspace' },
            { label: 'Manage workspace state', translations: { 'zh-CN': '管理工作区状态' }, slug: 'guides/workspace-state' },
            { label: 'Call the editor', translations: { 'zh-CN': '调用编辑器' }, slug: 'guides/outgoing-client' },
            { label: 'Progress & custom messages', translations: { 'zh-CN': '进度与自定义消息' }, slug: 'guides/progress-and-custom-messages' },
          ],
        },
        {
          label: 'Connect and embed',
          translations: { 'zh-CN': '连接与嵌入' },
          items: [
            { label: 'Choose a transport', translations: { 'zh-CN': '选择传输层' }, slug: 'guides/transports' },
            { label: 'Stdio & custom transports', translations: { 'zh-CN': 'Stdio 与自定义传输层' }, slug: 'guides/stdio-and-custom-transports' },
            { label: 'Build an LSP client', translations: { 'zh-CN': '构建 LSP 客户端' }, slug: 'guides/client-adoption' },
          ],
        },
        {
          label: 'Reliability',
          translations: { 'zh-CN': '可靠性' },
          items: [
            { label: 'Errors & cancellation', translations: { 'zh-CN': '错误与取消' }, slug: 'guides/errors-and-cancellation' },
            { label: 'Test protocol behavior', translations: { 'zh-CN': '测试协议行为' }, slug: 'guides/testing' },
            { label: 'Resources & observability', translations: { 'zh-CN': '资源与可观测性' }, slug: 'guides/operations' },
            { label: 'Deploy & troubleshoot', translations: { 'zh-CN': '部署与故障排查' }, slug: 'guides/deployment-and-troubleshooting' },
          ],
        },
        {
          label: 'Explore and reference',
          translations: { 'zh-CN': '探索与参考' },
          items: [
            { label: 'Feature servers', translations: { 'zh-CN': '功能示例服务器' }, slug: 'examples' },
            { label: 'API reference', translations: { 'zh-CN': 'API 参考' }, slug: 'reference' },
          ],
        },
      ],
    }),
  ],
});
