# lspf documentation site

The project site is built with Astro Starlight. Each language has a mirrored
directory under `src/content/docs/`, currently `en/` and `zh-cn/`. The root
URL redirects to the default English locale.

```console
npm install
npm run dev
```

Run `npm run check` to validate content and TypeScript, then `npm run build`
to create the production site in `dist/`. The Astro `site` and `base` values
target GitHub Pages at `meymchen.github.io/lspf`.
