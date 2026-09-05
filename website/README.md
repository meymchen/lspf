# lspf documentation site

The project site is built with VitePress. Each language has a mirrored directory
under `src/content/docs/`, currently `en/` and `zh-cn/`. English is published at
the site root, while other languages use a locale prefix.

```console
npm install
npm run dev
```

In VS Code, install the recommended Firefox Debug extension, then choose
**Debug website (Firefox)** from the Run and Debug view to start the development
server and open the site without entering a command.

Run `npm run check` to validate content invariants, then `npm run build`
to create and validate the production site in `dist/`. The VitePress site is
published at `https://lspf.dev` through GitHub Pages.

Canonical page links omit `.html` and a trailing slash. The production build
also emits redirect pages for trailing-slash URLs and the former `/en/` routes.
