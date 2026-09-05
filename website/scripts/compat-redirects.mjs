import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';

const SITE_ORIGIN = 'https://lspf.dev';
const LEGACY_PROJECT_BASE = 'lspf';

function pageRoute(page) {
  return page
    .replace(/(^|\/)index\.md$/, '$1')
    .replace(/\.md$/, '')
    .replace(/\/$/, '');
}

function redirectHtml(target) {
  const canonical = `${SITE_ORIGIN}${target}`;
  const scriptTarget = JSON.stringify(target);
  return `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta http-equiv="refresh" content="0;url=${target}">
    <meta name="robots" content="noindex">
    <link rel="canonical" href="${canonical}">
    <title>Redirecting…</title>
    <script>location.replace(${scriptTarget} + location.search + location.hash)</script>
  </head>
  <body><a href="${target}">Continue to ${target}</a></body>
</html>
`;
}

async function writeRedirect(outDir, outputPath, target) {
  const destination = join(outDir, ...outputPath.split('/'));
  await mkdir(dirname(destination), { recursive: true });
  await writeFile(destination, redirectHtml(target));
}

function addRedirect(redirects, outputPath, target) {
  const existingTarget = redirects.get(outputPath);
  if (existingTarget && existingTarget !== target) {
    throw new Error(`${outputPath} cannot redirect to both ${existingTarget} and ${target}`);
  }
  redirects.set(outputPath, target);
}

function canonicalTarget(route, isIndex) {
  if (!route) return '/';
  return isIndex ? `/${route}/` : `/${route}`;
}

function addPathAliases(redirects, path, target) {
  addRedirect(redirects, `${path}/index.html`, target);
  addRedirect(redirects, `${path}.html`, target);
}

export async function writeCompatibilityRedirects(siteConfig) {
  const redirects = new Map();

  for (const sourcePage of siteConfig.pages) {
    const publishedPage = siteConfig.rewrites.map[sourcePage] ?? sourcePage;
    const route = pageRoute(publishedPage);
    const isIndex = publishedPage === 'index.md' || publishedPage.endsWith('/index.md');
    const target = canonicalTarget(route, isIndex);

    if (route && !isIndex) {
      addRedirect(redirects, `${route}/index.html`, target);
    }

    const previousProjectRoute = route
      ? `${LEGACY_PROJECT_BASE}/${route}`
      : LEGACY_PROJECT_BASE;
    addPathAliases(redirects, previousProjectRoute, target);

    if (!sourcePage.startsWith('en/')) continue;

    const legacyEnglishRoute = route ? `en/${route}` : 'en';
    addPathAliases(redirects, legacyEnglishRoute, target);

    const previousEnglishRoute = route
      ? `${LEGACY_PROJECT_BASE}/en/${route}`
      : `${LEGACY_PROJECT_BASE}/en`;
    addPathAliases(redirects, previousEnglishRoute, target);
  }

  await Promise.all([...redirects].map(([outputPath, target]) =>
    writeRedirect(siteConfig.outDir, outputPath, target),
  ));
}
