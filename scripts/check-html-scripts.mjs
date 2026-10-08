import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const htmlPath = join(root, 'SlideShowPro.html');
const html = readFileSync(htmlPath, 'utf8');
const re = /<script\b([^>]*)>([\s\S]*?)<\/script>/gi;
const dir = mkdtempSync(join(tmpdir(), 'ssp-scripts-'));
let n = 0;
let failed = false;

try {
  for (const match of html.matchAll(re)) {
    const attrs = match[1] || '';
    const body = match[2] || '';
    if (/\bsrc\s*=/i.test(attrs)) continue;
    if (!body.trim()) continue;
    n += 1;
    const file = join(dir, `inline-${n}.js`);
    writeFileSync(file, body);
    const result = spawnSync(process.execPath, ['--check', file], { encoding: 'utf8' });
    if (result.status !== 0) {
      failed = true;
      const detail = `${result.stderr || ''}${result.stdout || ''}`.trim();
      console.error(`SlideShowPro.html inline script #${n} failed node --check`);
      if (detail) console.error(detail);
    }
  }
} finally {
  rmSync(dir, { recursive: true, force: true });
}

if (n === 0) {
  console.error('No inline scripts found in SlideShowPro.html');
  process.exit(1);
}
if (failed) process.exit(1);
console.log(`Checked ${n} inline script(s) in SlideShowPro.html`);
