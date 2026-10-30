// Fails if a shipped dependency's licence is not known to be compatible with GPL-3.0-or-later.
// Covers Rust crates (cargo metadata) and the npm packages bundled into the frontend.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

const ALLOWED = new Set([
  'MIT', 'MIT-0', 'Apache-2.0', 'Apache-2.0 WITH LLVM-exception', 'BSD-2-Clause', 'BSD-3-Clause',
  'ISC', 'Zlib', '0BSD', 'Unlicense', 'CC0-1.0', 'BSL-1.0', 'MPL-2.0', 'Unicode-3.0',
  'CDLA-Permissive-2.0', 'LGPL-2.1-or-later', 'GPL-3.0-or-later',
  // Fonts only: bundled as separate data files with their licences in public/licenses.
  'OFL-1.1',
]);

// Minimal SPDX: OR picks any allowed side, AND needs every side, parentheses group.
function allowed(expression) {
  const tokens = expression.replace(/\//g, ' OR ').match(/\(|\)|[^\s()]+(?: WITH [^\s()]+)?/g) ?? [];
  let i = 0;
  const term = () => {
    if (tokens[i] === '(') { i++; const value = or(); i++; return value; }
    return ALLOWED.has(tokens[i++]);
  };
  const and = () => { let value = term(); while (tokens[i] === 'AND') { i++; value = term() && value; } return value; };
  const or = () => { let value = and(); while (tokens[i] === 'OR') { i++; value = and() || value; } return value; };
  return tokens.length > 0 && or() && i === tokens.length;
}

const problems = [];
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--format-version', '1', '--locked'], { encoding: 'utf8', maxBuffer: 1 << 28 }));
for (const crate of metadata.packages) {
  if (!crate.license || !allowed(crate.license)) problems.push(`crate ${crate.name} ${crate.version}: ${crate.license ?? 'no licence field'}`);
}
const { dependencies = {} } = JSON.parse(readFileSync('package.json', 'utf8'));
for (const name of Object.keys(dependencies)) {
  const { license, version } = JSON.parse(readFileSync(`node_modules/${name}/package.json`, 'utf8'));
  if (!license || !allowed(license)) problems.push(`npm ${name} ${version}: ${license ?? 'no licence field'}`);
}
if (problems.length) {
  console.error(`Dependencies with licences not on the GPL-3.0-or-later allowlist:\n${problems.join('\n')}`);
  process.exit(1);
}
console.log(`${metadata.packages.length} crates and ${Object.keys(dependencies).length} npm packages have allowed licences.`);
