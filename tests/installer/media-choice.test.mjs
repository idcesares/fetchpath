// The installer may offer media tools, but setup never downloads them silently
// and never as part of Full (FP-099): an interactive setup shows an unticked
// checkbox on the installation type page, and only a ticked box downloads.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const hooks = readFileSync(new URL('../../apps/desktop/src-tauri/installer-hooks.nsh', import.meta.url), 'utf8');
const nsi = readFileSync(new URL('../../apps/desktop/src-tauri/installer.nsi', import.meta.url), 'utf8');
const post = hooks.match(/!macro NSIS_HOOK_POSTINSTALL\b([\s\S]*?)!macroend/)?.[1];

test('the type page offers the pinned media path as an unticked box with its consequence', () => {
  const page = nsi.match(/Function FpTypePage\n([\s\S]*?)FunctionEnd/)?.[1];
  assert.ok(page);
  assert.match(page, /NSD_CreateCheckBox\} 0 66u 100% 10u "Also download video and audio tools \(yt-dlp, FFmpeg\) from their publishers now\."/);
  assert.match(page, /Third-party licences apply, and Fetchpath checks their pinned SHA-256 values\. You can do this later in Settings\./);
  assert.ok(!/BM_SETCHECK \$FpCbMedia/.test(page), 'never preticked');
});

test('interactive setup downloads only when the box was ticked, after files are installed', () => {
  assert.ok(post);
  const gate = post.indexOf('${If} $FpMedia <> 1');
  const run = post.indexOf("nsExec::ExecToLog '\"$INSTDIR\\fetchpath.exe\" tools install --yes'");
  assert.ok(gate >= 0 && run > gate, 'the box gates the download');
  assert.ok(gate > post.indexOf('Delete "${FETCHPATH_UPDATE_HOLD}"'));
  assert.match(post, /yt-dlp and FFmpeg \(including ffprobe\)/);
  assert.match(post, /pinned SHA-256/);
  assert.match(post, /StrCmp \$R9 0 fetchpath_media_done[\s\S]*?retry from Settings/);
  assert.ok(!/MessageBox MB_YESNO\|MB_ICONQUESTION\|MB_DEFBUTTON2 "Set up video and audio tools/.test(post), 'the old question is gone');
});

test('quiet and passive setup skip the media download choice', () => {
  assert.ok(post);
  assert.match(post, /\$\{IfNot\} \$\{Silent\}\s+\$\{AndIf\} \$PassiveMode != 1/);
  assert.match(post, /\$\{If\} \$\{FileExists\} "\$APPDATA\\app\.fetchpath\.desktop\\media-tools\\yt-dlp\.exe"/);
  // The media variable is only ever set by the type page.
  const setters = [...nsi.matchAll(/StrCpy \$FpMedia (\d)/g)].map((m) => m[1]);
  assert.deepEqual([...new Set(setters)].sort(), ['0', '1']);
  assert.ok(nsi.match(/Function FpTypeLeave\n([\s\S]*?)FunctionEnd/)[1].includes('StrCpy $FpMedia 1'));
  assert.ok(!/Function FpInitSelection\n[\s\S]*?StrCpy \$FpMedia 1/.test(nsi));
});
