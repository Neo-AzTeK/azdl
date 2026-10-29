import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

function sources(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sources(path);
    return /\.(svelte|js|css)$/.test(entry.name) && !entry.name.endsWith('.test.js') ? [path] : [];
  });
}

const files = sources(new URL('.', import.meta.url).pathname).map((path) => [path, readFileSync(path, 'utf8')]);

describe('icon sources', () => {
  it('never draws icons with raw SVG markup or data URIs', () => {
    for (const [path, text] of files) {
      expect(text, path).not.toMatch(/<svg\b|<path\b|data:image\/svg/);
    }
  });

  it('passes iconset objects, not object-wrapped ones, in script data', () => {
    for (const [path, text] of files) {
      expect(text, path).not.toMatch(/\bicon:\s*\{icon[A-Z]\w*\}/);
    }
  });

  it('imports the outlined style whenever the iconset provides one', () => {
    const iconset = new URL('../node_modules/@ktibow/iconset-material-symbols/', import.meta.url).pathname;
    const available = new Set(readdirSync(iconset));
    for (const [path, text] of files) {
      for (const [, name] of text.matchAll(/iconset-material-symbols\/([a-z0-9-]+)'/g)) {
        if (!name.endsWith('-outline')) expect(available.has(`${name}-outline.js`), `${path}: ${name}`).toBe(false);
      }
    }
  });
});
