// Uninstall data removal (FP-100, A01, A12). The choice is the bundler's
// "Delete the application data" checkbox, or /DELETEAPPDATA for a silent
// uninstall. Removal runs only after the engine is stopped, touches only the
// two Fetchpath folders, and never follows a junction (NSIS's own RMDir /r
// does; measured with NSIS 3.11). The real uninstall runs are in
// tests/compatibility/windows.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const hooks = readFileSync(new URL('../../apps/desktop/src-tauri/installer-hooks.nsh', import.meta.url), 'utf8');
const config = JSON.parse(readFileSync(new URL('../../apps/desktop/src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
const script = fileURLToPath(new URL('../../apps/desktop/src-tauri/tools/remove-app-data.ps1', import.meta.url));

function macro(name) {
  const match = hooks.match(new RegExp(`!macro ${name}\\b([\\s\\S]*?)!macroend`));
  assert.ok(match, `${name} is defined`);
  return match[1].split('\n').map((line) => line.replace(/;.*$/, '').trim()).filter(Boolean).join('\n');
}

test('data is removed only when chosen, and never during an upgrade', () => {
  const plan = macro('FETCHPATH_PLAN_DATA_REMOVAL');
  assert.match(plan, /GetOptions\} \$CMDLINE "\/DELETEAPPDATA"/);
  assert.match(plan, /\$\{If\} \$DeleteAppDataCheckboxState = 1\n\$\{AndIf\} \$UpdateMode <> 1/);
  // Chosen or not, the bundler's RmDir /r must not run: it follows junctions.
  assert.ok(plan.includes('StrCpy $DeleteAppDataCheckboxState 0'));
  const remove = macro('FETCHPATH_REMOVE_DATA');
  assert.match(remove, /\n\$\{If\} \$FetchpathWipeData = 1/);
  assert.ok(!/RmDir|RMDir/.test(plan + remove), 'no NSIS recursive delete');
  assert.ok(!hooks.includes('MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Delete'), 'one question only');
});

test('the engine is stopped before the choice is acted on, and removal runs last', () => {
  const pre = macro('NSIS_HOOK_PREUNINSTALL').split('\n');
  assert.equal(pre[0], '!insertmacro FETCHPATH_STOP_ENGINE');
  assert.ok(pre.indexOf('!insertmacro FETCHPATH_PLAN_DATA_REMOVAL') > 0);
  // Deleting waits for POSTUNINSTALL: the bundler has closed the app and
  // removed the program files by then.
  assert.ok(!pre.includes('!insertmacro FETCHPATH_REMOVE_DATA'));
  assert.ok(macro('NSIS_HOOK_POSTUNINSTALL').includes('!insertmacro FETCHPATH_REMOVE_DATA'));
});

test('the script ships with the app, and the hook runs it from a copy', () => {
  assert.equal(config.bundle.resources['tools/remove-app-data.ps1'], 'tools/remove-app-data.ps1');
  assert.ok(macro('FETCHPATH_PLAN_DATA_REMOVAL').includes('CopyFiles /SILENT "$INSTDIR\\tools\\remove-app-data.ps1" "$PLUGINSDIR\\remove-app-data.ps1"'));
  assert.ok(macro('FETCHPATH_REMOVE_DATA').includes('-File "$PLUGINSDIR\\remove-app-data.ps1"'));
});

test('the script names only the two owned folders and checks for reparse points', () => {
  const text = readFileSync(script, 'utf8');
  assert.ok(text.includes("$leaf = 'app.fetchpath.desktop'"));
  assert.ok(text.includes('foreach ($base in @($Roaming, $Local))'));
  assert.ok(text.includes('[IO.FileAttributes]::ReparsePoint'));
  assert.ok(!/Remove-Item|\bGet-ChildItem\b/.test(text), 'Windows PowerShell 5.1 follows junctions in these');
  assert.ok(!/Downloads|Documents|USERPROFILE/.test(text.replace(/#<[\s\S]*?#>/, '')));
});

const windows = process.platform === 'win32';
function run(roaming, local) {
  return spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', script, '-Roaming', roaming, '-Local', local], { encoding: 'utf8' });
}
function junction(link, target) {
  const made = spawnSync('cmd.exe', ['/c', 'mklink', '/J', link, target], { encoding: 'utf8' });
  assert.equal(made.status, 0, made.stdout + made.stderr);
}

test('removal deletes both folders, deletes a junction inside as a link, and spares what it points at', { skip: !windows }, () => {
  const base = mkdtempSync(join(tmpdir(), 'fp-remove-'));
  try {
    const roaming = join(base, 'Roaming');
    const local = join(base, 'Local');
    const outside = join(base, 'Downloads');
    for (const dir of [join(roaming, 'app.fetchpath.desktop', 'media-tools'), join(local, 'app.fetchpath.desktop', 'EBWebView'), join(roaming, 'other-app'), outside]) {
      mkdirSync(dir, { recursive: true });
    }
    writeFileSync(join(outside, 'finished.iso'), 'keep');
    writeFileSync(join(roaming, 'other-app', 'settings.json'), 'keep');
    writeFileSync(join(roaming, 'app.fetchpath.desktop', 'queue-v1.json'), 'x');
    writeFileSync(join(local, 'app.fetchpath.desktop', 'EBWebView', 'data'), 'x');
    junction(join(roaming, 'app.fetchpath.desktop', 'media-tools', 'link'), outside);
    const result = run(roaming, local);
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.ok(!existsSync(join(roaming, 'app.fetchpath.desktop')));
    assert.ok(!existsSync(join(local, 'app.fetchpath.desktop')));
    assert.equal(readFileSync(join(outside, 'finished.iso'), 'utf8'), 'keep');
    assert.equal(readFileSync(join(roaming, 'other-app', 'settings.json'), 'utf8'), 'keep');
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

test('a root that is itself a junction is refused and nothing behind it is touched', { skip: !windows }, () => {
  const base = mkdtempSync(join(tmpdir(), 'fp-refuse-'));
  try {
    const roaming = join(base, 'Roaming');
    const local = join(base, 'Local');
    const elsewhere = join(base, 'Elsewhere');
    mkdirSync(roaming, { recursive: true });
    mkdirSync(join(local, 'app.fetchpath.desktop'), { recursive: true });
    mkdirSync(elsewhere);
    writeFileSync(join(elsewhere, 'precious.txt'), 'keep');
    junction(join(roaming, 'app.fetchpath.desktop'), elsewhere);
    const result = run(roaming, local);
    assert.equal(result.status, 3, result.stdout + result.stderr);
    assert.match(result.stdout, /Refused/);
    assert.equal(readFileSync(join(elsewhere, 'precious.txt'), 'utf8'), 'keep');
    assert.ok(!existsSync(join(local, 'app.fetchpath.desktop')), 'the other root is still removed');
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

test('every program under the install folder is stopped before data removal, and paths are passed in', () => {
  const stop = macro('FETCHPATH_STOP_ENGINE');
  assert.match(stop, /FETCHPATH_SETUP_DIR/);
  assert.match(stop, /StartsWith\(\$\$env:FETCHPATH_SETUP_DIR/);
  assert.match(macro('FETCHPATH_REMOVE_DATA'), /-Roaming "\$APPDATA" -Local "\$LOCALAPPDATA"/);
  assert.match(macro('FETCHPATH_REMOVE_DATA'), /SetErrorLevel 3/);
  assert.match(macro('FETCHPATH_PLAN_DATA_REMOVAL'), /SetErrorLevel 3/);
});

test('the update hold goes last and each root is handled on its own', { skip: !windows }, () => {
  const text = readFileSync(script, 'utf8');
  assert.ok(text.indexOf('Remove-Tree $root $true') < text.indexOf('[IO.File]::Delete($hold)'));
  assert.ok(text.indexOf('[IO.File]::Delete($hold)') < text.indexOf('[IO.Directory]::Delete($root, $false)'));
  const base = mkdtempSync(join(tmpdir(), 'fp-hold-'));
  try {
    const roaming = join(base, 'Roaming');
    mkdirSync(join(roaming, 'app.fetchpath.desktop'), { recursive: true });
    writeFileSync(join(roaming, 'app.fetchpath.desktop', 'engine-update-hold-v1'), 'x');
    const result = run(roaming, join(base, 'Local'));
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.match(result.stdout, /Not present/);
    assert.ok(!existsSync(join(roaming, 'app.fetchpath.desktop')));
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

test('each decision is logged, so a silent uninstall shows which branch ran', () => {
  assert.match(macro('FETCHPATH_PLAN_DATA_REMOVAL'), /FETCHPATH_LOG "plan: cmdline=\[\$CMDLINE\]/);
  assert.match(macro('FETCHPATH_PLAN_DATA_REMOVAL'), /FETCHPATH_LOG "plan: wipe=\$FetchpathWipeData"/);
  assert.match(macro('FETCHPATH_REMOVE_DATA'), /FETCHPATH_LOG "remove: wipe=\$FetchpathWipeData"/);
  assert.match(macro('FETCHPATH_REMOVE_DATA'), /FETCHPATH_LOG "remove: script exit=\$R9"/);
});
