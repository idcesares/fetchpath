// The installer template is a fork of Tauri's (FP-099, A01). The fork records the
// hash of the upstream template it was taken from; this test extracts the
// template that the installed Tauri CLI would use and fails when it differs, so
// a Tauri upgrade forces a reviewed re-merge instead of a silent drift.
// The CLI embeds the template in its native module; without an installed CLI
// (no `pnpm install` in apps/desktop) the comparison is skipped, the rest runs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const desktop = join(root, 'apps/desktop');
const fork = readFileSync(join(desktop, 'src-tauri/installer.nsi'), 'utf8');
const config = JSON.parse(readFileSync(join(desktop, 'src-tauri/tauri.conf.json'), 'utf8'));

function installedCli() {
  const store = join(desktop, 'node_modules/.pnpm');
  if (!existsSync(store)) return null;
  const folder = readdirSync(store).find((name) => name.startsWith('@tauri-apps+cli-win32-x64-msvc@'));
  if (!folder) return null;
  const version = folder.split('@').pop();
  const native = join(store, folder, 'node_modules/@tauri-apps/cli-win32-x64-msvc/cli.win32-x64-msvc.node');
  return existsSync(native) ? { version, native } : null;
}

function upstreamTemplate(native) {
  const text = readFileSync(native).toString('latin1');
  const start = text.indexOf('Unicode true\r\nManifestDPIAware');
  assert.ok(start >= 0, 'the NSIS template is in the CLI');
  const marker = text.indexOf('Function CreateOrUpdateDesktopShortcut', start + 30000);
  const end = text.indexOf('FunctionEnd', marker) + 'FunctionEnd'.length;
  return `${text.slice(start, end).replace(/\r\n/g, '\n')}\n`;
}

test('the bundle uses the forked template and the hooks', () => {
  assert.equal(config.bundle.windows.nsis.template, './installer.nsi');
  assert.equal(config.bundle.windows.nsis.installerHooks, './installer-hooks.nsh');
});

test('the fork names the upstream template it was taken from', () => {
  assert.match(fork, /@tauri-apps\/cli\n; 2\.\d+\.\d+ \(sha256 of the upstream template/);
  assert.match(fork, /^; [0-9a-f]{64}\)\.$/m);
});

test('the upstream template has not changed since the fork was taken', { skip: installedCli() ? false : 'the Tauri CLI is not installed (pnpm install in apps/desktop)' }, () => {
  const cli = installedCli();
  const recorded = fork.match(/^; ([0-9a-f]{64})\)\.$/m)[1];
  const version = fork.match(/@tauri-apps\/cli\n; (\d+\.\d+\.\d+) \(sha256/)[1];
  const actual = createHash('sha256').update(upstreamTemplate(cli.native), 'latin1').digest('hex');
  assert.equal(cli.version, version, `the CLI is ${cli.version}; the fork was taken from ${version}. Re-merge installer.nsi with the new upstream template, then update the header.`);
  assert.equal(actual, recorded, 'the upstream template differs from the one the fork was taken from. Re-merge installer.nsi, then update the hash in its header.');
});

test('the fork keeps every bundle-driven list and the upstream pages, in order', () => {
  for (const loop of ['resources_dirs', 'resources', 'binaries', 'resources_ancestors', 'file_associations', 'deep_link_protocols', 'languages', 'language_files']) {
    assert.ok(fork.includes(`{{#each ${loop}`), `${loop} is still rendered`);
  }
  const order = [
    '!insertmacro MUI_PAGE_WELCOME',
    'Page custom PageReinstall PageLeaveReinstall',
    'Page custom FpTypePage FpTypeLeave',
    '!insertmacro MUI_PAGE_COMPONENTS',
    '!insertmacro MUI_PAGE_DIRECTORY',
    '!insertmacro MUI_PAGE_STARTMENU Application $AppStartMenuFolder',
    '!insertmacro MUI_PAGE_INSTFILES',
    '!insertmacro MUI_PAGE_FINISH',
    '!insertmacro MUI_UNPAGE_CONFIRM',
    '!insertmacro MUI_UNPAGE_INSTFILES',
  ].map((page) => fork.indexOf(page));
  assert.ok(order.every((at) => at >= 0), order.join());
  assert.deepEqual(order, [...order].sort((a, b) => a - b), 'pages are in the documented order');
});

test('every upstream switch is still handled', () => {
  for (const option of ['"/P"', '"/NS"', '"/UPDATE"', '"/R"', '"/ARGS"']) {
    assert.ok(fork.includes(`\${GetOptions} $CMDLINE ${option}`), option);
  }
  assert.ok(fork.includes('"/DELETEAPPDATA"') || readFileSync(join(desktop, 'src-tauri/installer-hooks.nsh'), 'utf8').includes('"/DELETEAPPDATA"'));
});
