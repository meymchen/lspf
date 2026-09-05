import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join, relative, sep } from 'node:path';

const websiteRoot = new URL('../', import.meta.url);
const docsRoot = new URL('../src/content/docs/', import.meta.url);
const distRoot = new URL('../dist/', import.meta.url);
const siteOrigin = 'https://lspf.dev';

async function markdownFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(entries.map(async (entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return markdownFiles(path);
    return entry.name.endsWith('.md') && entry.name !== '_README.md' ? [path] : [];
  }));
  return nested.flat();
}

function routeFor(locale, sourcePath) {
  const withoutExtension = sourcePath.replace(/\.md$/, '').replace(/(^|\/)index$/, '$1');
  const route = withoutExtension.replace(/\/$/, '');
  if (locale === 'en') return route;
  return route ? `${locale}/${route}` : locale;
}

async function exists(relativePath) {
  try {
    await readFile(new URL(relativePath, distRoot));
    return true;
  } catch (error) {
    if (error?.code === 'ENOENT') return false;
    throw error;
  }
}

async function assertRedirect(relativePath, target) {
  const html = await readFile(new URL(relativePath, distRoot), 'utf8');
  assert.ok(
    html.includes(`content="0;url=${target}"`),
    `${relativePath} must redirect to ${target}`,
  );
  assert.ok(
    html.includes(`rel="canonical" href="${siteOrigin}${target}"`),
    `${relativePath} must identify ${target} as canonical`,
  );
}

function canonicalTarget(route, isIndex) {
  if (!route) return '/';
  return isIndex ? `/${route}/` : `/${route}`;
}

async function assertPathAliases(path, target) {
  for (const alias of [`${path}/index.html`, `${path}.html`]) {
    assert.equal(await exists(alias), true, `missing compatibility alias ${alias}`);
    await assertRedirect(alias, target);
  }
}

const canonicalFiles = [];
let expectedRustdocLinks = 0;
for (const locale of ['en', 'zh-cn']) {
  const localeRoot = new URL(`${locale}/`, docsRoot);
  const localePath = localeRoot.pathname;
  for (const path of await markdownFiles(localePath)) {
    const sourcePath = relative(localePath, path).split(sep).join('/');
    const source = await readFile(path, 'utf8');
    expectedRustdocLinks += source.match(/\]\(lspf::[^)]*\)/g)?.length ?? 0;
    const route = routeFor(locale, sourcePath);
    const isIndex = sourcePath === 'index.md';
    const target = canonicalTarget(route, isIndex);
    const canonicalFile = isIndex
      ? (locale === 'en' ? 'index.html' : `${locale}/index.html`)
      : `${route}.html`;
    assert.equal(await exists(canonicalFile), true, `missing canonical page ${canonicalFile}`);
    canonicalFiles.push(canonicalFile);

    if (!isIndex) {
      const slashAlias = `${route}/index.html`;
      assert.equal(await exists(slashAlias), true, `missing trailing-slash alias ${slashAlias}`);
      await assertRedirect(slashAlias, target);
    }

    if (locale === 'en') {
      const legacyRoute = route ? `en/${route}` : 'en';
      await assertPathAliases(legacyRoute, target);

      const previousEnglishRoute = route ? `lspf/en/${route}` : 'lspf/en';
      await assertPathAliases(previousEnglishRoute, target);
    }

    const previousProjectRoute = route ? `lspf/${route}` : 'lspf';
    await assertPathAliases(previousProjectRoute, target);
  }
}

const home = await readFile(new URL('index.html', distRoot), 'utf8');
assert.doesNotMatch(home, /http-equiv="refresh"/, 'English home must render at the site root');
const chineseHome = await readFile(new URL('zh-cn/index.html', distRoot), 'utf8');
assert.doesNotMatch(chineseHome, /http-equiv="refresh"/, 'Chinese home must render at /zh-cn/');

const rendered = (await Promise.all(canonicalFiles.map((path) =>
  readFile(new URL(path, distRoot), 'utf8'),
))).join('\n');
const rustdocLinks = rendered.match(/https:\/\/docs\.rs\/lspf\/latest\/lspf\/\?search=/g) ?? [];
assert.equal(rustdocLinks.length, expectedRustdocLinks, 'all rustdoc links must be rewritten');
for (const [, href] of rendered.matchAll(/href="([^"]+)"/g)) {
  if (!href.startsWith('/') || href.startsWith('//')) continue;
  const path = href.split(/[?#]/, 1)[0];
  assert.equal(path.startsWith('/lspf/'), false, `internal link must use the custom-domain root: ${href}`);
  if (path === '/' || path === '/zh-cn/') continue;
  assert.equal(path.endsWith('/'), false, `page link should omit its trailing slash: ${href}`);
}

for (const asset of ['favicon.svg', 'logo-mark.svg', 'logo.svg', 'logo-dark.svg']) {
  const source = await readFile(new URL(`public/${asset}`, websiteRoot));
  const built = await readFile(new URL(asset, distRoot));
  assert.deepEqual(built, source, `${asset} must be copied without modification`);
}

const sitemap = await readFile(new URL('sitemap.xml', distRoot), 'utf8');
assert.match(sitemap, /<loc>https:\/\/lspf\.dev\//, 'sitemap must use the custom domain');
assert.doesNotMatch(sitemap, /meymchen\.github\.io|https:\/\/lspf\.dev\/lspf(?:\/|<)/,
  'sitemap must not use the previous GitHub Pages base');
assert.doesNotMatch(sitemap, /https:\/\/lspf\.dev\/en(?:\/|<)/,
  'legacy English routes must not be canonical');

console.log(`Built-site check passed for ${canonicalFiles.length} canonical pages.`);
