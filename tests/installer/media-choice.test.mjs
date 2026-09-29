// The installer may offer media tools, but setup never downloads them silently.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const hooks = readFileSync(new URL('../../apps/desktop/src-tauri/installer-hooks.nsh', import.meta.url), 'utf8');

test('interactive setup offers the pinned media path after files are installed', () => {
  const post = hooks.match(/!macro NSIS_HOOK_POSTINSTALL\b([\s\S]*?)!macroend/)?.[1];
  assert.ok(post);
  const choice = post.indexOf('MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2');
  assert.ok(choice > post.indexOf('Delete "${FETCHPATH_UPDATE_HOLD}"'));
  assert.match(post, /yt-dlp and FFmpeg \(including ffprobe\)/);
  assert.match(post, /Unlicense and GPL-3\.0-or-later/);
  assert.match(post, /pinned SHA-256/);
  assert.match(post, /nsExec::ExecToLog '\"\$INSTDIR\\fetchpath\.exe\" tools install --yes'/);
  assert.match(post, /StrCmp \$R9 0 fetchpath_media_done[\s\S]*?retry from Settings/);
});

test('quiet and passive setup skip the media download choice', () => {
  const post = hooks.match(/!macro NSIS_HOOK_POSTINSTALL\b([\s\S]*?)!macroend/)?.[1];
  assert.ok(post);
  assert.match(post, /\$\{IfNot\} \$\{Silent\}\s+\$\{AndIf\} \$PassiveMode != 1/);
  assert.match(post, /\$\{If\} \$\{FileExists\} "\$APPDATA\\app\.fetchpath\.desktop\\media-tools\\yt-dlp\.exe"/);
  assert.match(post, /MessageBox MB_YESNO\|MB_ICONQUESTION\|MB_DEFBUTTON2/);
  assert.match(post, /IDNO fetchpath_media_done/);
});
