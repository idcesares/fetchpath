// The installer's PATH edit (apps/desktop/src-tauri/tools/user-path.ps1).
// Regression: the first version did this in NSIS, whose registry read returns
// an empty string for values over 1024 characters, and it wiped a real PATH.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('../../apps/desktop/src-tauri/tools/user-path.ps1', import.meta.url));
const dir = 'C:\\Users\\a b\\AppData\\Local\\Fetchpath';

function dryRun(action, current) {
  return execFileSync(
    'powershell.exe',
    ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', script,
      '-Action', action, '-Dir', dir, '-Current', current, '-DryRun'],
    { encoding: 'utf8' },
  ).replace(/\r?\n$/, '');
}

const longPath = Array.from({ length: 100 }, (_, i) => `%USERPROFILE%\\tools\\folder-with-a-long-name-${i}`).join(';');

test('adding keeps every existing character, even past 1024', { skip: process.platform !== 'win32' }, () => {
  assert.ok(longPath.length > 3000);
  assert.equal(dryRun('Add', longPath), `${longPath};${dir}`);
  assert.equal(dryRun('Add', 'C:\\a;'), `C:\\a;${dir}`);
  assert.equal(dryRun('Add', ''), dir);
});

test('adding twice changes nothing', { skip: process.platform !== 'win32' }, () => {
  assert.equal(dryRun('Add', `C:\\a;${dir.toUpperCase()}\\`), '<unchanged>');
});

test('removing restores the value that was there before adding', { skip: process.platform !== 'win32' }, () => {
  assert.equal(dryRun('Remove', `${longPath};${dir}`), longPath);
  assert.equal(dryRun('Remove', `C:\\a;${dir};C:\\b`), 'C:\\a;C:\\b');
  assert.equal(dryRun('Remove', dir), '');
});

test('removing leaves a PATH without the folder untouched', { skip: process.platform !== 'win32' }, () => {
  assert.equal(dryRun('Remove', longPath), '<unchanged>');
  assert.equal(dryRun('Remove', ''), '<unchanged>');
});
