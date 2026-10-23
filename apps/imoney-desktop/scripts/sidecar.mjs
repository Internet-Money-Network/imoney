// Builds the node and puts it where Tauri bundles it: src-tauri/binaries/imoney-node-<target>.
// Tauri names bundled programs by the platform they run on, so each platform's installer
// carries its own node. Set TARGET to cross-build (e.g. x86_64-apple-darwin).
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..', '..');
const target = process.env.TARGET || execFileSync('rustc', ['--print', 'host-tuple']).toString().trim();
const exe = target.includes('windows') ? '.exe' : '';

execFileSync('cargo', ['build', '--release', '-p', 'imoney-node', '--target', target], { cwd: root, stdio: 'inherit' });

const out = join(here, '..', 'src-tauri', 'binaries');
mkdirSync(out, { recursive: true });
copyFileSync(join(root, 'target', target, 'release', `imoney-node${exe}`), join(out, `imoney-node-${target}${exe}`));
console.log(`node for ${target} ready`);
