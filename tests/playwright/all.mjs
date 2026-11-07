// Entry for `node --test tests/playwright/`: Node 22 resolves the directory
// through package.json "main", so this loads every *.spec.mjs here.
import { readdirSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const dir = dirname(fileURLToPath(import.meta.url));
for (const f of readdirSync(dir).filter((f) => f.endsWith('.spec.mjs')).sort()) {
  await import(pathToFileURL(`${dir}/${f}`).href);
}
