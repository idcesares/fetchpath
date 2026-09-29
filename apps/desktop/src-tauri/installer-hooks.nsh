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

; The engine (FP-057). `fetchpath engine` may be running from $INSTDIR with
; downloads in progress, and a running executable cannot be replaced or
; deleted. Before either, the installed fetchpath.exe is asked to stop it:
; `engine stop --for-update` writes an update hold that keeps clients and a
; direct start from bringing an engine back, asks the engine to stop (it saves
; each download at its checkpoint), and returns once the engine has let go of
; its lock. The hooks then wait for fetchpath.exe itself to be free, since a
; command still running in a terminal also holds it, and only after 20 seconds
; end the fetchpath.exe processes still running from $INSTDIR, as the bundler
; does for the app window. Ending one is a crash the engine already recovers
; from. A 0.1.0
; fetchpath.exe does not know the command and had no engine. The hold is
; removed when setup finishes, and ignored after ten minutes if it never does.
; Its name matches EngineHome::update_hold_path.
!define FETCHPATH_UPDATE_HOLD "$APPDATA\app.fetchpath.desktop\engine-update-hold-v1"

!macro FETCHPATH_STOP_ENGINE
  Push $R8
  Push $R9
  ${If} ${FileExists} "$INSTDIR\fetchpath.exe"
    DetailPrint "Stopping the Fetchpath engine; downloads resume where they stopped."
    nsExec::ExecToLog '"$INSTDIR\fetchpath.exe" engine stop --for-update'
    Pop $R9
    StrCpy $R8 0
    fetchpath_engine_wait:
      ClearErrors
      FileOpen $R9 "$INSTDIR\fetchpath.exe" a
      ${IfNot} ${Errors}
        FileClose $R9
        Goto fetchpath_engine_free
      ${EndIf}
      IntOp $R8 $R8 + 1
      ${If} $R8 < 40
        Sleep 500
        Goto fetchpath_engine_wait
      ${EndIf}
    ; Only the copies running from this install: a fetchpath.exe elsewhere,
    ; such as a portable or development copy, is left alone. The path goes
    ; through the environment so no character in it needs quoting. Setup is
    ; a 32-bit process, so this is the 32-bit PowerShell, which cannot read
    ; a 64-bit process's Path; WMI's ExecutablePath works from either.
    DetailPrint "A fetchpath command was still running; closing it."
    System::Call 'Kernel32::SetEnvironmentVariable(t "FETCHPATH_SETUP_EXE", t "$INSTDIR\fetchpath.exe") i'
    nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "Get-CimInstance Win32_Process | Where-Object { $$_.ExecutablePath -eq $$env:FETCHPATH_SETUP_EXE } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force }"'
    Pop $R9
    Sleep 1000
    fetchpath_engine_free:
  ${EndIf}
  Pop $R9
  Pop $R8
!macroend

; A setup that stops early (Cancel, or a file it could not write) lifts the
; hold at once, rather than keep Fetchpath from starting for ten minutes. The
; bundler's template defines neither failure callback; Cancel goes through
; Modern UI, which owns .onUserAbort and calls the custom functions named here.
Function .onInstFailed
  Delete "${FETCHPATH_UPDATE_HOLD}"
FunctionEnd
Function un.onUninstFailed
  Delete "${FETCHPATH_UPDATE_HOLD}"
FunctionEnd
!define MUI_CUSTOMFUNCTION_ABORT FetchpathSetupCancelled
Function FetchpathSetupCancelled
  Delete "${FETCHPATH_UPDATE_HOLD}"
FunctionEnd
!define MUI_CUSTOMFUNCTION_UNABORT un.FetchpathSetupCancelled
Function un.FetchpathSetupCancelled
  Delete "${FETCHPATH_UPDATE_HOLD}"
FunctionEnd

; An uninstalled engine must not start at sign-in. The engine writes this
; value only when the person turned the setting on, and adds it again at its
; next start, so an upgrade that uninstalls first loses nothing for good.
!macro FETCHPATH_REMOVE_SIGN_IN
  Push $R9
  ReadRegStr $R9 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Fetchpath engine"
  ${If} $R9 == '"$INSTDIR\fetchpath.exe" engine'
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Fetchpath engine"
  ${EndIf}
  Pop $R9
!macroend

; The finish page says the command line came with the app (FP-044). This file is
; included before the bundler's pages, and the bundler leaves this text unset.
!define MUI_FINISHPAGE_TEXT "Fetchpath is installed.$\r$\n$\r$\nThe fetchpath command is installed too. Open a new terminal and type fetchpath --help to get started.$\r$\n$\r$\nClick Finish to close Setup."

!macro NSIS_HOOK_PREINSTALL
  !insertmacro FETCHPATH_STOP_ENGINE
!macroend

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Google\Chrome\NativeMessagingHosts\${FETCHPATH_HOST}" "" "$INSTDIR\${FETCHPATH_HOST}.chromium.json"
  WriteRegStr HKCU "Software\Microsoft\Edge\NativeMessagingHosts\${FETCHPATH_HOST}" "" "$INSTDIR\${FETCHPATH_HOST}.chromium.json"
  WriteRegStr HKCU "Software\Mozilla\NativeMessagingHosts\${FETCHPATH_HOST}" "" "$INSTDIR\${FETCHPATH_HOST}.firefox.json"
  DetailPrint "Registered the Fetchpath browser bridge for Chrome, Edge and Firefox."

  !insertmacro FETCHPATH_USER_PATH Add
  Delete "${FETCHPATH_UPDATE_HOLD}"

  ; Tauri calls this hook after the installed files are in place. Its finish
  ; page already uses both checkboxes (desktop shortcut and launch app), so an
  ; interactive setup asks here. Quiet and passive setup must never download
  ; optional software without a person choosing it.
  ${IfNot} ${Silent}
  ${AndIf} $PassiveMode != 1
    ; The guided installer puts all three executables in this data folder.
    ; An upgrade with them already present needs no new choice or download.
    ${If} ${FileExists} "$APPDATA\app.fetchpath.desktop\media-tools\yt-dlp.exe"
      ${If} ${FileExists} "$APPDATA\app.fetchpath.desktop\media-tools\ffmpeg.exe"
      ${AndIf} ${FileExists} "$APPDATA\app.fetchpath.desktop\media-tools\ffprobe.exe"
        Goto fetchpath_media_done
      ${EndIf}
      ${If} ${FileExists} "$APPDATA\app.fetchpath.desktop\media-tools\bin\ffmpeg.exe"
      ${AndIf} ${FileExists} "$APPDATA\app.fetchpath.desktop\media-tools\bin\ffprobe.exe"
        Goto fetchpath_media_done
      ${EndIf}
    ${EndIf}
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Set up video and audio tools now?$\r$\n$\r$\nFetchpath will download yt-dlp and FFmpeg (including ffprobe) over the internet. These third-party tools have their own licenses (Unlicense and GPL-3.0-or-later). Fetchpath checks their pinned SHA-256 values before using them. You can do this later in Settings." IDNO fetchpath_media_done
    DetailPrint "Setting up optional video and audio tools; download progress appears here."
    nsExec::ExecToLog '"$INSTDIR\fetchpath.exe" tools install --yes'
    Pop $R9
    StrCmp $R9 0 fetchpath_media_done
    DetailPrint "Media tools could not be set up (exit $R9). Fetchpath is installed; retry from Settings."
    MessageBox MB_OK|MB_ICONEXCLAMATION "Fetchpath is installed, but the optional video and audio tools could not be set up. You can retry from Settings."
    fetchpath_media_done:

    ; Chrome and Edge require the person to add an unpacked extension in the
    ; browser. Opening this folder only starts those steps; it grants no browser
    ; permission and automatic capture remains off until enabled in the popup.
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Show how to add the optional Fetchpath browser extension to Chrome or Edge?$\r$\n$\r$\nYour browser will ask you to load it. You can also find these steps later in Fetchpath Settings." IDNO fetchpath_browser_done
    ExecShell "open" "$INSTDIR\browser-extension"
    ${If} ${Errors}
      DetailPrint "Could not open the browser extension folder. Open it from Fetchpath Settings later."
    ${EndIf}
    MessageBox MB_OK|MB_ICONINFORMATION "In Chrome, open chrome://extensions; in Edge, open edge://extensions. Turn on Developer mode, choose Load unpacked, then select the browser-extension folder that Setup opened. To check the connection later, open Fetchpath Settings > Browser extension."
    fetchpath_browser_done:
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Before the files go, while fetchpath.exe and tools\user-path.ps1 are
  ; still installed.
  !insertmacro FETCHPATH_STOP_ENGINE
  !insertmacro FETCHPATH_USER_PATH Remove
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegKey HKCU "Software\Google\Chrome\NativeMessagingHosts\${FETCHPATH_HOST}"
  DeleteRegKey HKCU "Software\Microsoft\Edge\NativeMessagingHosts\${FETCHPATH_HOST}"
  DeleteRegKey HKCU "Software\Mozilla\NativeMessagingHosts\${FETCHPATH_HOST}"
  !insertmacro FETCHPATH_REMOVE_SIGN_IN
  Delete "${FETCHPATH_UPDATE_HOLD}"
!macroend
