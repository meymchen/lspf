import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const favicon = await readFile(new URL('../public/favicon.svg', import.meta.url), 'utf8');
const avatar = await readFile(new URL('../public/logo-mark.svg', import.meta.url), 'utf8');
const lightLogo = await readFile(new URL('../src/assets/logo.svg', import.meta.url), 'utf8');
const darkLogo = await readFile(new URL('../src/assets/logo-dark.svg', import.meta.url), 'utf8');

function markGeometry(source) {
  return [...source.matchAll(/<path\b[^>]*\bd="([^"]+)"[^>]*\/>/g)].map(([, path]) =>
    path.replace(/\s+/g, ' '),
  );
}

assert.match(favicon, /viewBox="0 0 48 48"/);
assert.match(avatar, /viewBox="0 0 48 48"/);
assert.match(lightLogo, /viewBox="0 0 148 48"/);
assert.match(darkLogo, /viewBox="0 0 148 48"/);
assert.deepEqual(markGeometry(lightLogo), markGeometry(darkLogo));
assert.deepEqual(markGeometry(avatar), markGeometry(lightLogo));
assert.deepEqual(markGeometry(favicon), markGeometry(avatar));

console.log('Brand asset check passed: avatar, favicon, and logo marks match.');
