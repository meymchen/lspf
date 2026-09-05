import { readdir, readFile } from 'node:fs/promises';
import { join, posix, relative, sep } from 'node:path';

const docsRoot = new URL('../src/content/docs/', import.meta.url);
const locales = ['en', 'zh-cn'];

async function markdownFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(entries.map(async (entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return markdownFiles(path);
    return /\.mdx?$/.test(entry.name) ? [path] : [];
  }));
  return nested.flat();
}

function headingShape(source) {
  return [...source.matchAll(/^(#{2,6})\s+\S.*$/gm)].map((match) => match[1].length);
}

function frontmatter(source, field) {
  const block = source.match(/^---\r?\n([\s\S]*?)\r?\n---/);
  return block?.[1].match(new RegExp(`^${field}:\\s*(.+)$`, 'm'))?.[1].trim();
}

const inventories = new Map();
for (const locale of locales) {
  const root = new URL(`${locale}/`, docsRoot);
  const rootPath = decodeURIComponent(root.pathname).replace(/^\/(?:[A-Za-z]:)/, (drive) => drive.slice(1));
  const files = await markdownFiles(rootPath);
  inventories.set(locale, new Map(await Promise.all(files.map(async (path) => [
    relative(rootPath, path).split(sep).join('/'),
    await readFile(path, 'utf8'),
  ]))));
}

const errors = [];
const allRoutes = new Set([...inventories.values()].flatMap((files) => [...files.keys()]));
for (const route of [...allRoutes].sort()) {
  const sources = locales.map((locale) => inventories.get(locale).get(route));
  for (const [index, source] of sources.entries()) {
    if (source === undefined) errors.push(`${route}: missing ${locales[index]} page`);
  }
  if (sources.some((source) => source === undefined)) continue;

  for (const [index, source] of sources.entries()) {
    for (const field of ['title', 'description']) {
      if (!frontmatter(source, field)) errors.push(`${route}: ${locales[index]} is missing ${field}`);
    }
  }

  const shapes = sources.map(headingShape);
  if (JSON.stringify(shapes[0]) !== JSON.stringify(shapes[1])) {
    errors.push(`${route}: heading hierarchy differs (${shapes[0]} versus ${shapes[1]})`);
  }
}

const chineseDeferrals = [
  /英文.{0,12}(完整|详细)/,
  /切换.{0,8}(英文|英语)/,
  /github\.com\/meymchen\/lspf\/blob\/main\/docs\/(?:guides|tutorials)\//,
];
for (const [route, source] of inventories.get('zh-cn')) {
  if (chineseDeferrals.some((pattern) => pattern.test(source))) {
    errors.push(`${route}: Chinese page defers user-facing content to another language or removed source docs`);
  }
}

for (const locale of locales) {
  const files = inventories.get(locale);
  const routes = new Set([...files.keys()].map((path) => {
    const withoutExtension = path.replace(/\.(?:md|mdx)$/, '');
    return withoutExtension === 'index' ? '' : withoutExtension.replace(/\/index$/, '');
  }));

  for (const [path, source] of files) {
    const sourceDirectory = posix.dirname(path);
    for (const match of source.matchAll(/\]\(([^\s)]+)(?:\s+"[^"]*")?\)/g)) {
      const destination = match[1];
      if (/^(?:[a-z][a-z+.-]*:|\/|#)/i.test(destination)) continue;
      const target = posix.normalize(posix.join(sourceDirectory, destination.split('#')[0]))
        .replace(/^\.\//, '')
        .replace(/\.(?:md|mdx)$/, '')
        .replace(/\/$/, '');
      if (target && !routes.has(target)) {
        errors.push(`${path}: ${locale} local link has no page: ${destination}`);
      }
    }
  }
}

if (errors.length) {
  console.error(`i18n parity check failed:\n- ${errors.join('\n- ')}`);
  process.exitCode = 1;
} else {
  console.log(`i18n parity check passed for ${allRoutes.size} paired pages.`);
}
