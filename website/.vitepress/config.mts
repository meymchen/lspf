import { fileURLToPath } from 'node:url';
import { defineConfig, type DefaultTheme } from 'vitepress';
import { writeCompatibilityRedirects } from '../scripts/compat-redirects.mjs';

const repository = 'https://github.com/meymchen/lspf';
const editPattern = `${repository}/edit/main/website/src/content/docs/:path`;

const englishSidebar: DefaultTheme.SidebarItem[] = [
  {
    text: 'Learn',
    items: [
      { text: 'Introduction', link: '/getting-started' },
      { text: 'Core concepts', link: '/concepts' },
    ],
  },
  {
    text: 'Tutorials',
    items: [
      { text: 'Build a language server', link: '/tutorials/server' },
      { text: 'Drive a language server', link: '/tutorials/client' },
    ],
  },
  {
    text: 'Build a server',
    items: [
      { text: 'Register features', link: '/guides/features-and-workspace' },
      { text: 'Manage workspace state', link: '/guides/workspace-state' },
      { text: 'Call the editor', link: '/guides/outgoing-client' },
      { text: 'Progress & custom messages', link: '/guides/progress-and-custom-messages' },
    ],
  },
  {
    text: 'Connect and embed',
    items: [
      { text: 'Choose a transport', link: '/guides/transports' },
      { text: 'Stdio & custom transports', link: '/guides/stdio-and-custom-transports' },
      { text: 'Build an LSP client', link: '/guides/client-adoption' },
      { text: 'Agents with LSP support', link: '/guides/agents' },
    ],
  },
  {
    text: 'Reliability',
    items: [
      { text: 'Errors & cancellation', link: '/guides/errors-and-cancellation' },
      { text: 'Test protocol behavior', link: '/guides/testing' },
      { text: 'Resources & observability', link: '/guides/operations' },
      { text: 'Deploy & troubleshoot', link: '/guides/deployment-and-troubleshooting' },
    ],
  },
  {
    text: 'Explore and reference',
    items: [
      { text: 'Feature servers', link: '/examples' },
      { text: 'API reference', link: '/reference' },
    ],
  },
];

const chineseSidebar: DefaultTheme.SidebarItem[] = [
  {
    text: '学习',
    items: [
      { text: '简介', link: '/zh-cn/getting-started' },
      { text: '核心概念', link: '/zh-cn/concepts' },
    ],
  },
  {
    text: '教程',
    items: [
      { text: '构建语言服务器', link: '/zh-cn/tutorials/server' },
      { text: '驱动语言服务器', link: '/zh-cn/tutorials/client' },
    ],
  },
  {
    text: '构建服务器',
    items: [
      { text: '注册功能', link: '/zh-cn/guides/features-and-workspace' },
      { text: '管理工作区状态', link: '/zh-cn/guides/workspace-state' },
      { text: '调用编辑器', link: '/zh-cn/guides/outgoing-client' },
      { text: '进度与自定义消息', link: '/zh-cn/guides/progress-and-custom-messages' },
    ],
  },
  {
    text: '连接与嵌入',
    items: [
      { text: '选择传输层', link: '/zh-cn/guides/transports' },
      { text: 'Stdio 与自定义传输层', link: '/zh-cn/guides/stdio-and-custom-transports' },
      { text: '构建 LSP 客户端', link: '/zh-cn/guides/client-adoption' },
      { text: '支持 LSP 的 Agent', link: '/zh-cn/guides/agents' },
    ],
  },
  {
    text: '可靠性',
    items: [
      { text: '错误与取消', link: '/zh-cn/guides/errors-and-cancellation' },
      { text: '测试协议行为', link: '/zh-cn/guides/testing' },
      { text: '资源与可观测性', link: '/zh-cn/guides/operations' },
      { text: '部署与故障排查', link: '/zh-cn/guides/deployment-and-troubleshooting' },
    ],
  },
  {
    text: '探索与参考',
    items: [
      { text: '功能示例服务器', link: '/zh-cn/examples' },
      { text: 'API 参考', link: '/zh-cn/reference' },
    ],
  },
];

const sharedThemeConfig: DefaultTheme.Config = {
  siteTitle: false,
  logo: {
    light: '/logo.svg',
    dark: '/logo-dark.svg',
    alt: 'lspf',
  },
  socialLinks: [
    { icon: 'github', link: repository, ariaLabel: 'GitHub' },
  ],
  editLink: {
    pattern: editPattern,
    text: 'Edit this page on GitHub',
  },
  lastUpdated: {
    text: 'Last updated',
    formatOptions: { dateStyle: 'medium', timeStyle: 'short' },
  },
  outline: { label: 'On this page', level: [2, 3] },
  externalLinkIcon: true,
  search: {
    provider: 'local',
    options: {
      locales: {
        'zh-cn': {
          translations: {
            button: {
              buttonText: '搜索',
              buttonAriaLabel: '搜索',
            },
            modal: {
              displayDetails: '显示详细列表',
              resetButtonTitle: '重置搜索',
              backButtonTitle: '关闭搜索',
              noResultsText: '没有结果',
              footer: {
                selectText: '选择',
                selectKeyAriaLabel: '确认',
                navigateText: '切换',
                navigateUpKeyAriaLabel: '上一个结果',
                navigateDownKeyAriaLabel: '下一个结果',
                closeText: '关闭',
                closeKeyAriaLabel: '退出',
              },
            },
          },
        },
      },
    },
  },
};

export default defineConfig({
  title: 'lspf',
  description: 'Language servers for IDEs and typed LSP clients for agent tools, built in Rust.',
  base: '/',
  srcDir: 'src/content/docs',
  srcExclude: ['_README.md'],
  outDir: 'dist',
  cacheDir: '.vitepress/cache',
  cleanUrls: true,
  lastUpdated: true,
  sitemap: {
    hostname: 'https://lspf.dev',
  },
  head: [
    ['link', { rel: 'icon', href: '/favicon.svg', type: 'image/svg+xml' }],
    ['meta', { name: 'theme-color', content: '#0c131c' }],
    ['meta', { property: 'og:type', content: 'website' }],
    ['meta', { property: 'og:site_name', content: 'lspf documentation' }],
  ],
  locales: {
    root: {
      label: 'English',
      lang: 'en',
      themeConfig: {
        nav: [
          { text: 'Architecture', link: '/concepts' },
          { text: 'Guides', link: '/getting-started' },
          { text: 'Examples', link: '/examples' },
          { text: 'Agents', link: '/guides/agents' },
        ],
      },
    },
    'zh-cn': {
      label: '简体中文',
      lang: 'zh-CN',
      link: '/zh-cn/',
      themeConfig: {
        sidebar: chineseSidebar,
        nav: [
          { text: '架构', link: '/zh-cn/concepts' },
          { text: '指南', link: '/zh-cn/getting-started' },
          { text: '示例', link: '/zh-cn/examples' },
          { text: 'Agent', link: '/zh-cn/guides/agents' },
        ],
        editLink: { pattern: editPattern, text: '在 GitHub 上编辑此页' },
        lastUpdated: {
          text: '最后更新于',
          formatOptions: { dateStyle: 'medium', timeStyle: 'short' },
        },
        outline: { label: '本页内容', level: [2, 3] },
        docFooter: { prev: '上一页', next: '下一页' },
        darkModeSwitchLabel: '主题',
        lightModeSwitchTitle: '切换到浅色主题',
        darkModeSwitchTitle: '切换到深色主题',
        sidebarMenuLabel: '菜单',
        returnToTopLabel: '返回顶部',
        langMenuLabel: '切换语言',
        skipToContentLabel: '跳到正文',
        notFound: {
          title: '页面未找到',
          quote: '这个地址没有对应的文档页面。',
          linkLabel: '前往首页',
          linkText: '返回首页',
        },
      },
    },
  },
  themeConfig: {
    ...sharedThemeConfig,
    sidebar: englishSidebar,
  },
  rewrites(id) {
    return id.startsWith('en/') ? id.slice('en/'.length) : id;
  },
  markdown: {
    config(md) {
      md.core.ruler.after('inline', 'lspf-rustdoc-links', (state) => {
        for (const token of state.tokens) {
          if (token.type === 'fence' && token.info.startsWith('rust,')) {
            token.info = 'rust';
          }

          for (const child of token.children ?? []) {
            if (child.type !== 'link_open') continue;
            const destination = child.attrGet('href');
            if (!destination?.startsWith('lspf::')) continue;

            const item = destination.slice('lspf::'.length);
            child.attrSet(
              'href',
              `https://docs.rs/lspf/latest/lspf/?search=${encodeURIComponent(item)}`,
            );
          }
        }
      });
    },
  },
  vite: {
    publicDir: fileURLToPath(new URL('../public', import.meta.url)),
  },
  buildEnd: writeCompatibilityRedirects,
});
