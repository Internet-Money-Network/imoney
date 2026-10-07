// Builds the SDK from src/index.ts: a browser bundle, CommonJS and ES module builds, and
// type declarations. Also refreshes the copy of the browser bundle shipped in the WooCommerce plugin.
import { build } from 'esbuild';
import { execFileSync } from 'node:child_process';
import { copyFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const common = { entryPoints: [join(here, 'src/index.ts')], bundle: true, target: 'es2020', logLevel: 'warning' };

await build({
  ...common,
  format: 'iife',
  globalName: 'IMoney',
  outfile: join(here, 'dist/imoney.js'),
  // Older integrations use the bare IMoneyClient global
  footer: { js: "if (typeof window !== 'undefined') { window.IMoneyClient = IMoney.IMoneyClient; }" },
});
await build({ ...common, format: 'cjs', outfile: join(here, 'dist/index.js') });
await build({ ...common, format: 'esm', outfile: join(here, 'dist/index.mjs') });

execFileSync(process.execPath, [join(here, 'node_modules/typescript/bin/tsc'), '-p', here], { stdio: 'inherit' });

copyFileSync(join(here, 'dist/imoney.js'), join(here, '../../plugins/imoney-payments-for-woocommerce/imoney.js'));
console.log('SDK built.');
