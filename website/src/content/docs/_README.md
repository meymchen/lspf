# Locale structure

Each locale owns one directory beneath this directory. English is the canonical
source for synchronization, not a fallback shown to readers. Every translated
directory mirrors its relative paths and filenames:

```text
docs/
├── en/
│   ├── index.mdx
│   ├── getting-started.md
│   └── tutorials/
└── zh-cn/
    ├── index.mdx
    ├── getting-started.md
    └── tutorials/
```

To add a language:

1. Add its BCP-47 locale to `locales` in `astro.config.mjs`.
2. Copy the `en/` tree to a directory whose name matches the locale key.
3. Translate the complete meaning, caveats, tables, and runnable workflow. A
   translated page must stand on its own; never tell readers to switch language
   or follow an English page for omitted material.
4. Add an optional file in `src/content/i18n/` only for custom interface strings.
5. Run `npm run check` and `npm run build`.

Matching paths associate translations in Starlight's language switcher. Keep code,
API names, and external destinations synchronized with the canonical English page.
`npm run check:i18n` rejects missing counterparts, different heading hierarchies,
broken local links, and wording that defers Chinese readers to an English version or
to the removed repository copies. This structural gate supplements paired editorial
review; it cannot determine whether a translation conveys the same meaning.

User-facing tutorials and guides belong here. Some English pages are also included by
the crate's doctest-only module so their Rust examples compile, but there is only one
Markdown source file. Repository development material, ADRs, release evidence, and
performance baselines remain under the root `docs/` directory.
