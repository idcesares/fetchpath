// FP-102 installation consent and removal ordering. Configuration mutation is
// tested by apps/cli/tests/agent_setup.rs; real packaging by agent-installer.ps1.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
const read = name => readFileSync(new URL(`../../apps/desktop/src-tauri/${name}`, import.meta.url), 'utf8');
const nsi = read('installer.nsi');
const hooks = read('installer-hooks.nsh');
const macro = name => hooks.match(new RegExp(`!macro ${name}\\b([\\s\\S]*?)!macroend`))[1];
const fn = name => nsi.match(new RegExp(`Function ${name}\\b([\\s\\S]*?)FunctionEnd`))[1];

test('agent registration requires separate explicit choices and no grants', () => {
  const init = fn('FpInitSelection');
  assert.match(init, /StrCpy \$FpAgentCodex 0/);
  assert.match(init, /StrCpy \$FpAgentClaude 0/);
  assert.match(init, /GetOptions\} \$CMDLINE "\/AGENTHOSTS="/);
  assert.match(init, /\/AGENTHOSTS requires the AI agents component/);
  const page = fn('FpAgentsPage');
  assert.match(page, /\$PassiveMode = 1/);
  assert.match(page, /Call FpAgentsSelected/);
  assert.match(page, /SearchPath \$0 "codex\.exe"/);
  assert.match(page, /SearchPath \$0 "claude\.exe"/);
  const post = macro('NSIS_HOOK_POSTINSTALL');
  assert.match(post, /\$FpAgentCodex = 1[\s\S]*?"add codex"[\s\S]*?"check codex"/);
  assert.match(post, /\$FpAgentClaude = 1[\s\S]*?"add claude-code"[\s\S]*?"check claude-code"/);
  assert.ok(!/agents grant|agents auto|hub on|lan on/.test(post));
  assert.ok(post.indexOf('Delete "${FETCHPATH_UPDATE_HOLD}"') < post.indexOf('"check codex"'));
});

test('component removal and uninstall clean owned connections before binary/data removal', () => {
  assert.match(macro('NSIS_HOOK_POSTINSTALL'), /\$\{Else\}\s*!insertmacro FETCHPATH_AGENT_COMMAND "cleanup"/);
  const pre = macro('NSIS_HOOK_PREUNINSTALL');
  assert.ok(pre.indexOf('FETCHPATH_STOP_ENGINE') < pre.indexOf('"cleanup"'));
  assert.match(pre, /\$UpdateMode <> 1[\s\S]*?"cleanup"/);
  assert.match(pre, /\$FpReplacement <> 1[\s\S]*?"cleanup"/);
  assert.match(fn('PageLeaveReinstall'), /\$FpVerCmp <> 0[\s\S]*?"\$R1 \/REPLACE"/);
  assert.ok(pre.indexOf('"cleanup"') < pre.indexOf('FETCHPATH_PLAN_DATA_REMOVAL'));
  const un = nsi.slice(nsi.indexOf('Section Uninstall'));
  assert.ok(un.indexOf('NSIS_HOOK_PREUNINSTALL') < un.indexOf('Delete "$INSTDIR'));
  const command = macro('FETCHPATH_AGENT_COMMAND');
  assert.match(command, /\$R9 != 0[\s\S]*?Delete "\$\{FETCHPATH_UPDATE_HOLD\}"[\s\S]*?SetErrorLevel 11[\s\S]*?Quit/);
});
