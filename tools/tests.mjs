// Run repository tests without traversing ignored build or agent scratch trees.
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const inventory = spawnSync('git', [
  'ls-files', '-z', '--cached', '--others', '--exclude-standard', '--',
  '*.test.mjs', '*.test.js', '*.test.cjs',
], { cwd: root, encoding: 'utf8' });
if (inventory.status !== 0) {
  process.stderr.write(inventory.stderr || 'Could not list repository tests.\n');
  process.exit(inventory.status ?? 1);
}
const tests = [...new Set(inventory.stdout.split('\0').filter(Boolean))];
if (!tests.length) {
  process.stderr.write('No repository tests found.\n');
  process.exit(1);
}
const result = spawnSync(process.execPath, ['--test', '--', ...tests], {
  cwd: root, stdio: 'inherit',
});
process.exit(result.status ?? 1);
