// Setup may guide the person to the extension; the browser must load it.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const hooks = readFileSync(new URL('../../apps/desktop/src-tauri/installer-hooks.nsh', import.meta.url), 'utf8');
const post = hooks.match(/!macro NSIS_HOOK_POSTINSTALL\b([\s\S]*?)!macroend/)?.[1];

test('browser choice is explicit and opens only the bundled folder', () => {
  assert.ok(post);
  assert.match(post, /MessageBox MB_YESNO\|MB_ICONQUESTION\|MB_DEFBUTTON2 "Show how to add the optional Fetchpath browser extension/);
  assert.match(post, /browser will ask you to load it/);
  assert.match(post, /IDNO fetchpath_browser_done\s+ExecShell "open" "\$INSTDIR\\browser-extension"/);
  assert.match(post, /chrome:\/\/extensions.*edge:\/\/extensions.*Developer mode.*Load unpacked/);
  assert.match(post, /Fetchpath Settings > Browser extension/);
  assert.doesNotMatch(post, /--install-extension|ExtensionInstallForcelist/);
});

test('silent and passive setup bypass both optional choices', () => {
  assert.ok(post);
  const interactive = post.indexOf('${IfNot} ${Silent}');
  const passive = post.indexOf('${AndIf} $PassiveMode != 1', interactive);
  const browser = post.indexOf('Show how to add the optional Fetchpath browser extension', passive);
  const end = post.lastIndexOf('${EndIf}');
  assert.ok(interactive >= 0 && passive > interactive && browser > passive && end > browser);
});
