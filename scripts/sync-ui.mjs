import { copyFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const src = join(root, 'SlideShowPro.html');
const destDir = join(root, 'ui');
const dest = join(destDir, 'index.html');
mkdirSync(destDir, { recursive: true });
copyFileSync(src, dest);
copyFileSync(join(root, 'slidex-core.js'), join(destDir, 'slidex-core.js'));
console.log('Synced SlideShowPro.html -> ui/index.html');
