// Use the backend's own Miniflare dependency and migrations, with an ephemeral D1 DB.
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';

const backend = resolve(process.argv[2] ?? '../astraea-sync');
const require = createRequire(join(backend, 'package.json'));
const { Miniflare, convertV4MiniflareOptions } = require('miniflare');
function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: 'inherit', ...options });
    child.on('error', reject);
    child.on('exit', code => code === 0 ? resolve() : reject(new Error(`${command} exited ${code}`)));
  });
}
await run('npm', ['run', 'build'], { cwd: backend });
const mf = new Miniflare(convertV4MiniflareOptions({
  modules: true, script: readFileSync(join(backend, 'dist/index.js'), 'utf8'),
  compatibilityDate: '2026-10-26', host: '127.0.0.1', port: 0,
  d1Databases: { DB: 'nothingtodo-contract-test' },
}));
try {
  const db = await mf.getD1Database('DB');
  for (const file of ['0001_initial.sql', '0002_token_metadata.sql']) {
    const sql = readFileSync(join(backend, 'migrations', file), 'utf8');
    for (const line of sql.split('\n').filter(line => line.trim() && !line.startsWith('--'))) await db.exec(line);
  }
  const now = new Date().toISOString();
  await db.prepare('INSERT INTO accounts VALUES (?, ?, ?)').bind('test-account', 'NothingToDo test', now).run();
  // Public test fixture token, valid only in this throwaway database.
  const hash = createHash('sha256').update(`as_${'a'.repeat(43)}`).digest('hex');
  await db.prepare('INSERT INTO api_tokens(token_hash, account_id, created_at) VALUES (?, ?, ?)').bind(hash, 'test-account', now).run();
  const url = await mf.ready;
  await run('cargo', ['test', '-p', 'nothingtodo-core', '--test', 'sync_contract', '--', '--ignored'], {
    env: { ...process.env, NOTHINGTODO_TEST_API: url.origin },
  });
} finally { await mf.dispose(); }
