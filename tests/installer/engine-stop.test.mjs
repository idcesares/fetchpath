// Setup and a running engine (FP-057, A12). Files are replaced or removed only
// after the running engine was asked to stop, and the update hold that keeps a
// new one from starting is named as the engine names it and lifted afterwards.
// The real install, upgrade and uninstall runs are
// tests/compatibility/windows/engine-lifecycle.ps1.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const hooks = readFileSync(new URL('../../apps/desktop/src-tauri/installer-hooks.nsh', import.meta.url), 'utf8');
const launch = readFileSync(new URL('../../crates/fetchpath-protocol/src/launch.rs', import.meta.url), 'utf8');

function macro(name) {
  const match = hooks.match(new RegExp(`!macro ${name}\\b([\\s\\S]*?)!macroend`));
  assert.ok(match, `${name} is defined`);
  return match[1].split('\n').map((line) => line.replace(/;.*$/, '').trim()).filter(Boolean);
}

test('the engine is stopped before setup touches installed files', () => {
  assert.deepEqual(macro('NSIS_HOOK_PREINSTALL'), ['!insertmacro FETCHPATH_STOP_ENGINE']);
  // First in the uninstaller too: PATH removal runs a script from $INSTDIR,
  // and the bundler deletes files right after this hook.
  assert.equal(macro('NSIS_HOOK_PREUNINSTALL')[0], '!insertmacro FETCHPATH_STOP_ENGINE');
});

test('stopping waits for fetchpath.exe to be free, then ends only copies from this install', () => {
  const body = macro('FETCHPATH_STOP_ENGINE').join('\n');
  const stop = body.indexOf('engine stop --for-update');
  const wait = body.indexOf('FileOpen $R9 "$INSTDIR\\fetchpath.exe" a');
  const kill = body.indexOf('Stop-Process -Id');
  assert.ok(stop >= 0 && wait > stop && kill > wait, body);
  assert.match(body, /SetEnvironmentVariable\(t "FETCHPATH_SETUP_EXE", t "\$INSTDIR\\fetchpath\.exe"\)/);
  // WMI, not Get-Process: setup's 32-bit PowerShell sees no Path for a
  // 64-bit process, so a Path match silently ends nothing.
  assert.match(body, /Get-CimInstance Win32_Process \| Where-Object \{ \$\$_\.ExecutablePath -eq \$\$env:FETCHPATH_SETUP_EXE \}/);
  assert.ok(!body.includes('KillProcess'), 'no end-by-name');
});

test('a setup that stops early lifts the hold', () => {
  assert.ok(hooks.includes('!define MUI_CUSTOMFUNCTION_ABORT FetchpathSetupCancelled'));
  assert.ok(hooks.includes('!define MUI_CUSTOMFUNCTION_UNABORT un.FetchpathSetupCancelled'));
  for (const callback of ['.onInstFailed', 'un.onUninstFailed', 'FetchpathSetupCancelled', 'un.FetchpathSetupCancelled']) {
    const escaped = callback.replace(/\./g, '\\.');
    const match = hooks.match(new RegExp(`Function ${escaped}\\n([\\s\\S]*?)FunctionEnd`));
    assert.ok(match, `${callback} is defined`);
    assert.ok(match[1].includes('Delete "${FETCHPATH_UPDATE_HOLD}"'), callback);
  }
});

test('the update hold is the one the engine honors, and setup lifts it', () => {
  const name = launch.match(/fn update_hold_path[\s\S]*?join\("([^"]+)"\)/)[1];
  assert.ok(hooks.includes(`!define FETCHPATH_UPDATE_HOLD "$APPDATA\\app.fetchpath.desktop\\${name}"`));
  assert.ok(macro('NSIS_HOOK_POSTINSTALL').includes('Delete "${FETCHPATH_UPDATE_HOLD}"'));
  assert.ok(macro('NSIS_HOOK_POSTUNINSTALL').includes('Delete "${FETCHPATH_UPDATE_HOLD}"'));
});

test('uninstall removes the sign-in start only when it points at this install', () => {
  const body = macro('FETCHPATH_REMOVE_SIGN_IN').join('\n');
  assert.match(body, /\$\{If\} \$R9 == '"\$INSTDIR\\fetchpath\.exe" engine'\n\s*DeleteRegValue HKCU/);
  assert.ok(macro('NSIS_HOOK_POSTUNINSTALL').includes('!insertmacro FETCHPATH_REMOVE_SIGN_IN'));
});
