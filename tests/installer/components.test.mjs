// Installer components (FP-099, A01, A12, A13), checked on the forked template and
// the hooks as text. The behaviour itself is run in Windows Sandbox
// (tests/compatibility/windows/sandbox-lifecycle.ps1 -Components ...). The design
// is docs/architecture/specs/2026-10-02-installer-components-design.md.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = (path) => readFileSync(new URL(`../../apps/desktop/src-tauri/${path}`, import.meta.url), 'utf8');
const nsi = read('installer.nsi');
const hooks = read('installer-hooks.nsh');

// A "block" is a Function or Section, from its header to its end.
function blocks(text) {
  const found = new Map();
  const pattern = /^(Function|Section)\s+("[^"]*"|\S+)[^\n]*\n([\s\S]*?)^(FunctionEnd|SectionEnd)$/gm;
  for (const match of text.matchAll(pattern)) {
    found.set(`${match[1]} ${match[2].replace(/"/g, '')}`, match[3]);
  }
  return found;
}
const all = blocks(nsi);
function block(name) {
  const body = all.get(name);
  assert.ok(body !== undefined, `${name} is defined (have: ${[...all.keys()].join(', ')})`);
  return body;
}
const code = (text) => text.split('\n').map((line) => line.replace(/;.*$/, '').trim()).filter(Boolean).join('\n');
function macro(name) {
  const match = hooks.match(new RegExp(`!macro ${name}\\b([\\s\\S]*?)!macroend`));
  assert.ok(match, `${name} is defined`);
  return code(match[1]);
}

test('every use of the desktop exe name is accounted for', () => {
  const owners = new Set();
  for (const [name, body] of all) {
    if (code(body).includes('${MAINBINARYNAME}')) owners.add(name);
  }
  // Each of these is keyed on the Desktop component, as the spec's table says:
  //  - the desktop section and its Run entry, the shortcut functions the last
  //    section calls only with Desktop, the desktop launch after /R,
  //  - difference removal and uninstall, which delete the desktop's own file,
  //  - disk derivation, which looks for the desktop's own file,
  //  - Core, which must not remove it as an "old main binary".
  assert.deepEqual([...owners].sort(), [
    'Function .onInstSuccess',
    'Function CreateOrUpdateDesktopShortcut',
    'Function CreateOrUpdateStartMenuShortcut',
    'Function FpDeriveFromDisk',
    'Function FpRemoveDeselected',
    'Function FpRemoveDesktopShortcuts',
    'Function RunMainBinary',
    'Section Core',
    'Section Desktop app',
    'Section Uninstall',
  ]);
});

test('the engine exe, not the desktop exe, identifies the install', () => {
  const core = code(block('Section Core'));
  assert.ok(core.includes('WriteRegStr SHCTX "${UNINSTKEY}" "MainBinaryName" "fetchpath.exe"'));
  assert.ok(core.includes('"DisplayIcon" "$\\"$INSTDIR\\fetchpath.exe$\\""'));
  // The old-name cleanup never removes the engine or the desktop exe, and never runs for /UPDATE.
  assert.match(core, /\$\{AndIf\} \$OldMainBinaryName != "fetchpath\.exe"\n\$\{AndIf\} \$OldMainBinaryName != "\$\{MAINBINARYNAME\}\.exe"\n\$\{AndIf\} \$UpdateMode <> 1\nDelete "\$INSTDIR\\\$OldMainBinaryName"/);
  // "Is it still installed" after the old uninstaller asks about fetchpath.exe.
  assert.ok(code(nsi).includes('${OrIf} ${FileExists} "$INSTDIR\\fetchpath.exe"'));
  // The bundler's running-app prompt knew only the desktop exe; the hook stops everything instead.
  assert.ok(!/^\s*!insertmacro CheckIfAppIsRunning/m.test(nsi));
  assert.equal(macro('NSIS_HOOK_PREUNINSTALL').split('\n')[0], '!insertmacro FETCHPATH_STOP_ENGINE');
});

test('the desktop exe, WebView2, shortcuts and Run exist only with the Desktop component', () => {
  const desktop = code(block('Section Desktop app'));
  assert.match(desktop, /ReadRegStr \$4 HKLM "SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients/);
  assert.ok(desktop.includes('File "${MAINBINARYSRCPATH}"'));
  // Nothing else copies it.
  assert.equal([...code(nsi).matchAll(/File "\$\{MAINBINARYSRCPATH\}"/g)].length, 1);
  assert.equal([...code(nsi).matchAll(/Section "WebView2"|Section WebView2/g)].length, 0, 'WebView2 is part of Desktop');
  const finish = code(block('Section -Finish'));
  assert.match(finish, /\$\{If\} \$\{SectionIsSelected\} \$\{SecDesktop\}\nCall CreateOrUpdateDesktopShortcut/);
  assert.match(code(block('Function FpUpdateStartMenuEntries')), /^\$\{If\} \$\{SectionIsSelected\} \$\{SecDesktop\}\nCall CreateOrUpdateStartMenuShortcut/);
  // /R launches only the desktop app; a terminal-only install has no window.
  assert.match(code(block('Function .onInstSuccess')), /\$\{If\} \$\{SectionIsSelected\} \$\{SecDesktop\}\n\$\{GetOptions\} \$CMDLINE "\/ARGS" \$R0\nnsis_tauri_utils::RunAsUser/);
  // The finish page hides "Run Fetchpath" and the desktop shortcut box without Desktop.
  const show = code(block('Function FpFinishShow'));
  assert.match(show, /\$\{IfNot\} \$\{SectionIsSelected\} \$\{SecDesktop\}\nSendMessage[\s\S]*ShowWindow \$mui\.FinishPage\.ShowReadme \$\{SW_HIDE\}/);
});

test('the Terminal entry is made without Desktop, targets fetchpath.exe and is removed otherwise', () => {
  const entry = code(block('Function FpUpdateTerminalEntry'));
  assert.match(entry, /\$\{If\} \$UpdateMode = 1\nReturn/);
  assert.match(entry, /\$\{If\} \$\{SectionIsSelected\} \$\{SecCli\}\n\$\{AndIfNot\} \$\{SectionIsSelected\} \$\{SecDesktop\}/);
  assert.match(entry, /CreateShortcut "\$\{FP_TERMINAL_LNK\}" "\$INSTDIR\\fetchpath\.exe"/);
  assert.match(entry, /IsShortcutTarget "\$\{FP_TERMINAL_LNK\}" "\$INSTDIR\\fetchpath\.exe"/);
  assert.match(code(block('Section Uninstall')), /"\$SMPROGRAMS\\\$\{PRODUCTNAME\} Terminal\.lnk" "\$INSTDIR\\fetchpath\.exe"/);
});

test('file ownership: Core, Browser integration and Torrent helper copy their own files', () => {
  const owners = {
    Core: ['core'], 'Browser integration': ['browser'], 'Torrent helper': ['torrent'],
  };
  for (const [section, [owner]] of Object.entries(owners)) {
    const body = code(block(`Section ${section}`));
    assert.ok(body.includes(`!insertmacro FETCHPATH_BINARY "${owner}"`), `${section} copies its binaries`);
  }
  // The classification: the host is the browser's, the helper the torrent's, everything else Core's.
  assert.match(nsi, /!if "\$\{ONAME\}" == "fetchpath-browser-host\.exe"\n\s+!define FP_BIN_OWNER "browser"\n\s+!else if "\$\{ONAME\}" == "fetchpath-torrent-helper\.exe"\n\s+!define FP_BIN_OWNER "torrent"\n\s+!else\n\s+!define FP_BIN_OWNER "core"/);
  assert.match(nsi, /"browser-extension\\" FP_RES_REST/);
  assert.match(nsi, /"com\.fetchpath\.browser\." FP_RES_REST/);
  // Terminal and AI agents copy nothing; they are entry points.
  for (const section of ['Terminal (CLI and TUI)', 'AI agents (MCP)']) {
    assert.ok(!/File /.test(code(block(`Section ${section}`))), `${section} copies no file`);
  }
});

test('Core is required, one usable interface is required, and MCP never stands alone', () => {
  assert.ok(code(block('Section Core')).startsWith('SectionIn RO'));
  const leave = code(block('Function FpComponentsLeave'));
  assert.match(leave, /\$\{IfNot\} \$\{SectionIsSelected\} \$\{SecDesktop\}\n\$\{AndIfNot\} \$\{SectionIsSelected\} \$\{SecCli\}\nMessageBox MB_OK\|MB_ICONEXCLAMATION "Choose the app or the terminal so you can manage Fetchpath\."\nAbort/);
  assert.match(code(block('Function .onSelChange')), /\$\{If\} \$\{SectionIsSelected\} \$\{SecMcp\}\n\$\{AndIfNot\} \$\{SectionIsSelected\} \$\{SecDesktop\}\n\$\{AndIfNot\} \$\{SectionIsSelected\} \$\{SecCli\}\n!insertmacro SelectSection \$\{SecCli\}/);
  // Web is not part of this build: there is no section for it.
  assert.ok(!/Section\s+"Web/i.test(nsi));
});

test('the pages: type then components, Custom only, never in a quiet or passive setup', () => {
  const type = code(block('Function FpTypePage'));
  assert.ok(type.startsWith('${IfThen} $PassiveMode = 1 ${|} Abort ${|}'));
  assert.ok(type.includes('${IfThen} $FpSkipSelect = 1 ${|} Abort ${|}'));
  assert.match(type, /Full \(recommended\): the app, the terminal command, AI agent support, browser integration and the torrent helper\. About 37 MiB\./);
  assert.match(type, /Custom: choose what to install\./);
  assert.match(type, /Also download video and audio tools \(yt-dlp, FFmpeg\) from their publishers now\./);
  // The media box is unticked unless the person ticks it.
  assert.ok(!/BM_SETCHECK \$FpCbMedia/.test(type));
  const pre = code(block('Function FpComponentsPre'));
  assert.ok(pre.includes('${IfThen} $FpType == "full" ${|} Abort ${|}'));
  assert.ok(pre.includes('${IfThen} $PassiveMode = 1 ${|} Abort ${|}'));
  // Same version: Change components is preselected; Repair keeps the stored components.
  const reinstall = code(block('Function PageReinstall'));
  assert.match(reinstall, /\$\{NSD_CreateFirstRadioButton\} 30u 46u -30u 8u "Change components"/);
  assert.match(reinstall, /SendMessage \$FpRbChange \$\{BM_SETCHECK\}/);
  assert.match(code(block('Function PageLeaveReinstall')), /\$\{If\} \$FpReinstallChoice = 2\nStrCpy \$FpSkipSelect 1/);
});

test('/COMPONENTS: names, full, exit 10, and never with /UPDATE', () => {
  const init = code(block('Function FpInitSelection'));
  assert.match(init, /\$\{If\} \$UpdateMode <> 1\nClearErrors\n\$\{GetOptions\} \$CMDLINE "\/COMPONENTS=" \$R0/);
  assert.match(init, /\$\{If\} \$FpList == "full"\nStrCpy \$FpType "full"\nCall FpSelAllVars/);
  assert.match(init, /StrCpy \$FpType "custom"/);
  // A switch with no value is a mistake, not "no switch".
  assert.ok(init.includes('"/COMPONENTS needs a value'));
  const parse = code(block('Function FpParseList'));
  for (const name of ['core', 'desktop', 'cli', 'mcp', 'browser', 'torrent']) {
    assert.ok(parse.includes(`\${If} $FpTok == "${name}"`) || parse.includes(`\${ElseIf} $FpTok == "${name}"`), name);
  }
  assert.ok(parse.includes('${ElseIf} $FpTok == "web"\nStrCpy $FpReason "web is not part of this build"'));
  assert.ok(parse.includes('StrCpy $FpReason "unknown component [$FpTok]"'));
  // An empty entry (desktop,,cli, a leading or trailing comma) is refused, not skipped: WordFind would skip it.
  assert.ok(parse.includes('StrCpy $FpReason "the component list has an empty entry"'));
  assert.ok(!parse.includes('WordFind'));
  assert.ok(parse.includes('StrCpy $FpReason "mcp needs desktop or cli"'));
  assert.ok(parse.includes('StrCpy $FpReason "no usable interface: choose desktop or cli"'));
  // The refusal ends setup with code 10 before any section runs: it is in .onInit's call tree only.
  const refuse = code(block('Function FpRefuseSelection'));
  assert.match(refuse, /SetErrorLevel 10\nQuit$/);
  const callers = [...all].filter(([, body]) => body.includes('Call FpRefuseSelection')).map(([name]) => name);
  assert.deepEqual(callers, ['Function FpInitSelection']);
  assert.ok(code(block('Function .onInit')).includes('Call FpInitSelection'));
  // No quiet auto-add of Terminal: the parse refuses, the page rule is in .onSelChange only.
  assert.ok(!/SelectSection/.test(parse + init));
});

test('the selection is stored with the uninstall entry, canonical and sorted', () => {
  const write = code(block('Function FpWriteSelection'));
  assert.ok(write.includes('WriteRegStr SHCTX "${UNINSTKEY}" "FetchpathInstallType" "$FpType"'));
  assert.ok(write.includes('WriteRegStr SHCTX "${UNINSTKEY}" "FetchpathComponents" "$FpList"'));
  assert.ok(write.includes('WriteRegDWORD SHCTX "${UNINSTKEY}" "FetchpathComponentsSchema" 1'));
  const order = [...write.matchAll(/FP_APPEND "(\w+)"/g)].map((m) => m[1]);
  assert.deepEqual(order, ['browser', 'cli', 'core', 'desktop', 'mcp', 'torrent']);
  assert.deepEqual(order, [...order].sort());
  // Written by the last section, after every component and before the hooks' post step.
  const finish = code(block('Section -Finish'));
  assert.ok(finish.indexOf('Call FpWriteSelection') >= 0 && finish.indexOf('Call FpWriteSelection') < finish.indexOf('NSIS_HOOK_POSTINSTALL'));
});

test('a stored selection that cannot be used is derived from disk and never fatal', () => {
  const stored = code(block('Function FpReadStored'));
  // Before FP-099 there were no values: that install has everything this build has.
  assert.match(stored, /\$\{If\} \$FpOldType == ""\n\$\{AndIf\} \$FpList == ""\nStrCpy \$FpOldType "full"\nCall FpSelAllVars\nReturn/);
  assert.ok(stored.includes('${If} $1 > 1\nStrCpy $FpReason "the stored components use a newer format ($1)"'));
  assert.ok(stored.includes('StrCpy $FpReason "the stored install type is not full or custom"'));
  assert.match(stored, /!insertmacro FETCHPATH_INSTALL_LOG "stored selection unusable \(\$FpReason\), reading the install folder"\nCall FpDeriveFromDisk/);
  assert.ok(!/Quit|Abort|SetErrorLevel/.test(stored + code(block('Function FpDeriveFromDisk'))));
  const derive = code(block('Function FpDeriveFromDisk'));
  for (const file of ['${MAINBINARYNAME}.exe', 'fetchpath-browser-host.exe', 'fetchpath-torrent-helper.exe']) {
    assert.ok(derive.includes(`"$INSTDIR\\${file}"`), file);
  }
  assert.ok(derive.includes('-Action Test -Dir "$INSTDIR"'));
  // Terminal if no interface is found.
  assert.match(derive, /\$\{If\} \$FpSelDesktop = 0\n\$\{AndIf\} \$FpSelCli = 0\nStrCpy \$FpSelCli 1/);
});

test('an existing install keeps its selection unless /COMPONENTS says otherwise', () => {
  const init = code(block('Function FpInitSelection'));
  const read = init.indexOf('Call FpReadStored');
  const parse = init.indexOf('/COMPONENTS=');
  assert.ok(read > 0 && parse > read, 'the stored selection is read first and /COMPONENTS replaces it');
  assert.ok(init.includes('StrCpy $FpOldDesktop $FpSelDesktop'));
  assert.ok(init.endsWith('Call FpApplySelection'));
});

test('deselecting removes only that component, after the engine stops, and never for /UPDATE', () => {
  const core = code(block('Section Core'));
  assert.ok(core.indexOf('NSIS_HOOK_PREINSTALL') >= 0 && core.indexOf('NSIS_HOOK_PREINSTALL') < core.indexOf('Call FpRemoveDeselected'));
  assert.ok(core.indexOf('Call FpRemoveDeselected') < core.indexOf('FETCHPATH_RESOURCE "core"'), 'removal precedes copying');
  const removal = code(block('Function FpRemoveDeselected'));
  assert.match(removal, /^\$\{If\} \$UpdateMode = 1\n\$\{OrIf\} \$FpHasOld <> 1\nReturn/);
  assert.match(removal, /\$\{If\} \$FpOldDesktop = 1\n\$\{AndIfNot\} \$\{SectionIsSelected\} \$\{SecDesktop\}[\s\S]*?Delete "\$INSTDIR\\\$\{MAINBINARYNAME\}\.exe"\nCall FpRemoveDesktopShortcuts/);
  assert.match(removal, /\$\{If\} \$FpOldBrowser = 1\n\$\{AndIfNot\} \$\{SectionIsSelected\} \$\{SecBrowser\}[\s\S]*?FETCHPATH_REMOVE_HOST_KEYS\nDelete "\$INSTDIR\\fetchpath-browser-host\.exe"[\s\S]*?FETCHPATH_RESOURCE_DELETE "browser"[\s\S]*?RMDir "\$INSTDIR\\browser-extension"/);
  assert.match(removal, /\$\{If\} \$FpOldTorrent = 1\n\$\{AndIfNot\} \$\{SectionIsSelected\} \$\{SecTorrent\}\nDetailPrint "[^"]*"\nDelete "\$INSTDIR\\fetchpath-torrent-helper\.exe"/);
  // Data, grants, history, media tools and the WebView2 profile are never touched by a change.
  assert.ok(!/APPDATA|media-tools|EBWebView|RMDir \/r|RmDir \/r|app\.fetchpath/i.test(removal + code(block('Function FpRemoveDesktopShortcuts'))));
  // The hold is stopped by the same hook for every install except /UPDATE's own ordering (unchanged).
  assert.equal(macro('NSIS_HOOK_PREINSTALL'), '!insertmacro FETCHPATH_STOP_ENGINE');
});

test('registrations follow the selection', () => {
  const post = macro('NSIS_HOOK_POSTINSTALL');
  assert.match(post, /^\$\{If\} \$\{SectionIsSelected\} \$\{SecBrowser\}\nWriteRegStr HKCU "Software\\Google\\Chrome\\NativeMessagingHosts\\\$\{FETCHPATH_HOST\}"/);
  assert.match(post, /\$\{Else\}\n!insertmacro FETCHPATH_REMOVE_HOST_KEYS\n\$\{EndIf\}/);
  assert.match(post, /\$\{If\} \$\{SectionIsSelected\} \$\{SecCli\}\n\$\{OrIf\} \$\{SectionIsSelected\} \$\{SecMcp\}\n!insertmacro FETCHPATH_USER_PATH Add\n\$\{Else\}\n!insertmacro FETCHPATH_USER_PATH Remove\n\$\{EndIf\}/);
  // Uninstall still removes every key and the PATH entry.
  assert.ok(macro('NSIS_HOOK_POSTUNINSTALL').includes('!insertmacro FETCHPATH_REMOVE_HOST_KEYS'));
  assert.ok(macro('NSIS_HOOK_PREUNINSTALL').includes('!insertmacro FETCHPATH_USER_PATH Remove'));
  const keys = macro('FETCHPATH_REMOVE_HOST_KEYS');
  assert.equal([...keys.matchAll(/DeleteRegKey HKCU/g)].length, 3);
});

test('installing is never consent: no grants, serving, sharing, sign-in or tool download by default', () => {
  const commands = [...`${nsi}\n${hooks}`.matchAll(/nsExec::Exec\w+ '([^']*)'/g)].map((m) => m[1]);
  assert.ok(commands.length >= 5, 'the commands setup runs were found');
  const allowed = [
    /^"\$INSTDIR\\fetchpath\.exe" engine stop --for-update$/,
    /^"\$INSTDIR\\fetchpath\.exe" tools install --yes$/,
    /user-path\.ps1" -Action (\$\{ACTION\}|Test) -Dir "\$INSTDIR"$/,
    /remove-app-data\.ps1/,
    /Get-CimInstance Win32_Process/,
  ];
  for (const command of commands) {
    assert.ok(allowed.some((pattern) => pattern.test(command)), `unexpected command: ${command}`);
  }
  assert.ok(!/agents (grant|revoke)|lan (on|pair)|mcp --|settings |approve/.test(commands.join('\n')));
  // The one download needs the person's tick, in an interactive setup.
  const post = macro('NSIS_HOOK_POSTINSTALL');
  assert.match(post, /\$\{IfNot\} \$\{Silent\}\n\$\{AndIf\} \$PassiveMode != 1\n\$\{If\} \$FpMedia <> 1\nGoto fetchpath_media_done/);
  // Nothing writes an agent host's configuration or the sign-in Run value (the engine owns it).
  assert.ok(!/CurrentVersion\\Run" "Fetchpath engine" /.test(`${nsi}\n${hooks}`.replace(/DeleteRegValue[^\n]*/g, '')));
  assert.ok(!/claude_desktop_config|\.codex|mcp\.json/.test(`${nsi}\n${hooks}`));
});

test('quiet and passive setup show nothing and download nothing', () => {
  const post = macro('NSIS_HOOK_POSTINSTALL');
  // Both optional questions sit inside the interactive guard.
  const guard = post.indexOf('${IfNot} ${Silent}');
  assert.ok(guard > 0);
  assert.ok(post.indexOf('tools install --yes') > guard);
  assert.ok(post.indexOf('MessageBox') > guard);
  assert.ok(post.indexOf('ExecShell') > guard);
  // Refusing a selection is silent too: the message box is skipped and the code is 10.
  assert.match(code(block('Function FpRefuseSelection')), /\$\{IfNot\} \$\{Silent\}\n\$\{AndIf\} \$PassiveMode <> 1\nMessageBox/);
});

test('without Desktop the finish page unticks and guards Run and the desktop shortcut', () => {
  const show = code(block('Function FpFinishShow'));
  assert.match(show, /\$\{IfNot\} \$\{SectionIsSelected\} \$\{SecDesktop\}\nSendMessage \$mui\.FinishPage\.Run \$\{BM_SETCHECK\} \$\{BST_UNCHECKED\} 0\nSendMessage \$mui\.FinishPage\.ShowReadme \$\{BM_SETCHECK\} \$\{BST_UNCHECKED\} 0/);
  assert.match(code(block('Function RunMainBinary')), /^\$\{If\} \$FpDesktopOn = 1\nnsis_tauri_utils::RunAsUser/);
  assert.match(code(block('Function CreateOrUpdateDesktopShortcut')), /^\$\{If\} \$FpDesktopOn <> 1\nReturn/);
});

test('Back from Repair then Change shows the selection pages again', () => {
  assert.match(code(block('Function PageLeaveReinstall')), /\$\{If\} \$FpVerCmp = 0\nStrCpy \$FpSkipSelect 0\n/);
});

test('a stale Terminal Start entry is handled outside the Start menu block', () => {
  const finish = code(block('Section -Finish'));
  const end = finish.indexOf('!insertmacro MUI_STARTMENU_WRITE_END');
  assert.ok(end > 0 && finish.indexOf('Call FpUpdateTerminalEntry') > end);
  assert.ok(!code(block('Function FpUpdateStartMenuEntries')).includes('FpUpdateTerminalEntry'));
});
