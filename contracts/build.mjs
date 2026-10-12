// Compiles the contracts in src/ to out/<Name>.json (ABI and bytecode).
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const solc = createRequire(import.meta.url)('solc');
const sources = {};
for (const file of readdirSync('src')) {
  sources[file] = { content: readFileSync(`src/${file}`, 'utf8') };
}
const output = JSON.parse(
  solc.compile(
    JSON.stringify({
      language: 'Solidity',
      sources,
      settings: {
        optimizer: { enabled: true, runs: 200 },
        // Ganache's chain predates the newest opcodes; the contract needs none of them
        evmVersion: 'london',
        outputSelection: { '*': { '*': ['abi', 'evm.bytecode.object'] } },
      },
    })
  )
);
const problems = (output.errors ?? []).filter((e) => e.severity === 'error' || e.severity === 'warning');
for (const problem of problems) console.error(problem.formattedMessage);
if (problems.length) process.exit(1);

mkdirSync('out', { recursive: true });
for (const file of Object.keys(output.contracts)) {
  for (const [name, contract] of Object.entries(output.contracts[file])) {
    writeFileSync(`out/${name}.json`, JSON.stringify({ abi: contract.abi, bytecode: '0x' + contract.evm.bytecode.object }, null, 1));
  }
}
console.log('compiled', Object.values(output.contracts).flatMap(Object.keys).join(', '));
