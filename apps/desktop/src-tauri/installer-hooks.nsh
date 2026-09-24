; Fetchpath installer hooks.
;
; Referenced from tauri.conf.json as `bundle.windows.nsis.installerHooks`.
;
; User data on uninstall. The person decides, once, and only the two folders
; Fetchpath created for its own data can be removed:
;
;   %APPDATA%\app.fetchpath.desktop        queue, settings, browser inbox, media-tools\
;   %LOCALAPPDATA%\app.fetchpath.desktop   the WebView2 profile
;
; Downloaded files are never touched, wherever they were saved.
;
; The question is the bundler's own "Delete the application data" checkbox on
; the uninstall confirmation page: unticked by default, ignored during an
; upgrade, and removing exactly those two folders. FP-030 had added a second
; question here, a Yes/No box shown after that page. Asking twice let the
; answers contradict each other, and ticking the box then answering No still
; deleted the data, so the second question was removed on 23 September 2026.
; A silent uninstall keeps the data: nobody is present to choose, and keeping
; is the recoverable answer.

; Browser capture (FP-036). The host manifests are installed beside
; fetchpath-browser-host.exe with a relative `path`, which Chrome, Edge and
; Firefox all resolve against the manifest's own folder on Windows. Only these
; three per-user keys are written, and the uninstaller removes exactly them.
!define FETCHPATH_HOST "com.fetchpath.browser"

; Command line (FP-037). The install folder is added to the per-user PATH so
; `fetchpath` works in a new terminal. That edit is made by tools\user-path.ps1,
; never here: an NSIS registry read returns an EMPTY string for any value longer
; than NSIS_MAX_STRLEN (1024), so a long PATH looks empty, and an earlier version
; of this file wiped a real PATH that way. The script reads the raw value with
; no length limit and refuses any write that would shorten or empty it.
!macro FETCHPATH_USER_PATH ACTION
  Push $R9
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\tools\user-path.ps1" -Action ${ACTION} -Dir "$INSTDIR"'
  Pop $R9
  StrCmp $R9 0 +2
  DetailPrint "Could not update your PATH for the fetchpath command ($R9). Add $INSTDIR to it yourself."
  Pop $R9
  ; HWND_BROADCAST, WM_SETTINGCHANGE: new terminals pick up the change.
  SendMessage 0xFFFF 0x001A 0 "STR:Environment" /TIMEOUT=5000
!macroend

; The finish page says the command line came with the app (FP-044). This file is
; included before the bundler's pages, and the bundler leaves this text unset.
!define MUI_FINISHPAGE_TEXT "Fetchpath is installed.$\r$\n$\r$\nThe fetchpath command is installed too. Open a new terminal and type fetchpath --help to get started.$\r$\n$\r$\nClick Finish to close Setup."

!macro NSIS_HOOK_PREINSTALL
!macroend

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Google\Chrome\NativeMessagingHosts\${FETCHPATH_HOST}" "" "$INSTDIR\${FETCHPATH_HOST}.chromium.json"
  WriteRegStr HKCU "Software\Microsoft\Edge\NativeMessagingHosts\${FETCHPATH_HOST}" "" "$INSTDIR\${FETCHPATH_HOST}.chromium.json"
  WriteRegStr HKCU "Software\Mozilla\NativeMessagingHosts\${FETCHPATH_HOST}" "" "$INSTDIR\${FETCHPATH_HOST}.firefox.json"
  DetailPrint "Registered the Fetchpath browser bridge for Chrome, Edge and Firefox."

  !insertmacro FETCHPATH_USER_PATH Add
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Before the files go, while tools\user-path.ps1 is still installed.
  !insertmacro FETCHPATH_USER_PATH Remove
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegKey HKCU "Software\Google\Chrome\NativeMessagingHosts\${FETCHPATH_HOST}"
  DeleteRegKey HKCU "Software\Microsoft\Edge\NativeMessagingHosts\${FETCHPATH_HOST}"
  DeleteRegKey HKCU "Software\Mozilla\NativeMessagingHosts\${FETCHPATH_HOST}"
!macroend
