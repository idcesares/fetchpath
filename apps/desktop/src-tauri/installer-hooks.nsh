; Fetchpath installer hooks.
;
; Referenced from tauri.conf.json as `bundle.windows.nsis.installerHooks`.
;
; User data on uninstall (FP-100). The person decides, once, and only the two
; folders Fetchpath created for its own data can be removed:
;
;   %APPDATA%\app.fetchpath.desktop        queue, settings, rules, agent grants,
;                                          secrets, browser inbox, media-tools\,
;                                          terminal preferences, locks
;   %LOCALAPPDATA%\app.fetchpath.desktop   the WebView2 profile, LAN pairing
;
; and the registry key HKCU\Software\Fetchpath contributors that records the
; install folder. Downloaded files are never touched, wherever they were saved.
;
; The question is the bundler's own "Delete the application data" checkbox on
; the uninstall confirmation page: unticked by default and ignored during an
; upgrade. FP-030 had added a second question here; asking twice let the answers
; contradict each other, so it was removed on 23 September 2026. A silent
; uninstall keeps the data (nobody is present to choose, and keeping is the
; recoverable answer) unless it is started with /DELETEAPPDATA, which is the
; checkbox ticked for a person who cannot click it.
;
; The bundler would remove the two folders with `RmDir /r`, and NSIS 3.11's
; RMDir /r follows junctions (measured: a junction inside the tree had the
; folder it pointed at emptied). So when the choice is made, this file takes it
; over: PREUNINSTALL records it and clears the bundler's flag, and
; POSTUNINSTALL, after the engine and the app have stopped and the program
; files are gone, runs tools\remove-app-data.ps1. That script removes only the
; two exact folders, refuses one that is itself a junction, and deletes a link
; inside as a link without entering it.
Var FetchpathWipeData

; One line per decision, in $TEMP\fetchpath-uninstall.log, so a silent uninstall
; shows which branch ran. Never holds paths of user files or secrets.
!macro FETCHPATH_LOG TEXT
  Push $R7
  ClearErrors
  FileOpen $R7 "$TEMP\fetchpath-uninstall.log" a
  ${IfNot} ${Errors}
    FileSeek $R7 0 END
    FileWrite $R7 "${TEXT}$\r$\n"
    FileClose $R7
  ${EndIf}
  Pop $R7
!macroend

; Components (FP-099). installer.nsi has one section per component; these hooks
; are section-aware (they ask which components are selected) and stay the single
; place for the engine stop, the PATH edit, the native-host keys and the data
; removal. Design: docs/architecture/specs/2026-10-02-installer-components-design.md.
;
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
  ; Every other program running from this install (the browser host, the torrent
  ; and media helpers, the app) must be gone before files or data are removed.
  ; Only paths under $INSTDIR\; a copy elsewhere is left alone.
  System::Call 'Kernel32::SetEnvironmentVariable(t "FETCHPATH_SETUP_DIR", t "$INSTDIR\") i'
  nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "Get-CimInstance Win32_Process | Where-Object { $$_.ExecutablePath -and $$_.ExecutablePath.StartsWith($$env:FETCHPATH_SETUP_DIR, [System.StringComparison]::OrdinalIgnoreCase) } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force }"'
  Pop $R9
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

; The finish page text is set in installer.nsi (FpFinishShow): it depends on the
; components that were installed.

; One line per decision, in $TEMP\fetchpath-install.log, so a quiet install shows
; why a selection was refused or derived. Never holds user data or secrets.
!macro FETCHPATH_INSTALL_LOG TEXT
  Push $R7
  ClearErrors
  FileOpen $R7 "$TEMP\fetchpath-install.log" a
  ${IfNot} ${Errors}
    FileSeek $R7 0 END
    FileWrite $R7 "${TEXT}$\r$\n"
    FileClose $R7
  ${EndIf}
  Pop $R7
!macroend

; The three per-user native-messaging keys belong to Browser integration (FP-099):
; written when it is selected, removed when it is deselected and on uninstall.
!macro FETCHPATH_REMOVE_HOST_KEYS
  DeleteRegKey HKCU "Software\Google\Chrome\NativeMessagingHosts\${FETCHPATH_HOST}"
  DeleteRegKey HKCU "Software\Microsoft\Edge\NativeMessagingHosts\${FETCHPATH_HOST}"
  DeleteRegKey HKCU "Software\Mozilla\NativeMessagingHosts\${FETCHPATH_HOST}"
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro FETCHPATH_STOP_ENGINE
!macroend

; Runs once, after every component is in place and the selection is known
; (installer.nsi, section "-Finish"). Installing is never consent: nothing here
; grants an agent, starts serving, enables sharing, or downloads a tool unless
; the person ticked the media box on the installation type page.
!macro NSIS_HOOK_POSTINSTALL
  ${If} ${SectionIsSelected} ${SecBrowser}
    WriteRegStr HKCU "Software\Google\Chrome\NativeMessagingHosts\${FETCHPATH_HOST}" "" "$INSTDIR\${FETCHPATH_HOST}.chromium.json"
    WriteRegStr HKCU "Software\Microsoft\Edge\NativeMessagingHosts\${FETCHPATH_HOST}" "" "$INSTDIR\${FETCHPATH_HOST}.chromium.json"
    WriteRegStr HKCU "Software\Mozilla\NativeMessagingHosts\${FETCHPATH_HOST}" "" "$INSTDIR\${FETCHPATH_HOST}.firefox.json"
    DetailPrint "Registered the Fetchpath browser bridge for Chrome, Edge and Firefox."
  ${Else}
    !insertmacro FETCHPATH_REMOVE_HOST_KEYS
  ${EndIf}

  ; The PATH entry exists while Terminal or AI agents is selected.
  ${If} ${SectionIsSelected} ${SecCli}
  ${OrIf} ${SectionIsSelected} ${SecMcp}
    !insertmacro FETCHPATH_USER_PATH Add
  ${Else}
    !insertmacro FETCHPATH_USER_PATH Remove
  ${EndIf}
  Delete "${FETCHPATH_UPDATE_HOLD}"

  ; Tauri calls this hook after the installed files are in place. Quiet and
  ; passive setup must never download optional software without a person
  ; choosing it, so only an interactive setup acts on the media box.
  ${IfNot} ${Silent}
  ${AndIf} $PassiveMode != 1
    ; The guided installer puts all three executables in this data folder.
    ; An upgrade with them already present needs no new download.
    ${If} $FpMedia <> 1
      Goto fetchpath_media_done
    ${EndIf}
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
    DetailPrint "Downloading yt-dlp and FFmpeg (including ffprobe) from their publishers; Fetchpath checks their pinned SHA-256 values. Progress appears here."
    nsExec::ExecToLog '"$INSTDIR\fetchpath.exe" tools install --yes'
    Pop $R9
    StrCmp $R9 0 fetchpath_media_done
    DetailPrint "Media tools could not be set up (exit $R9). Fetchpath is installed; retry from Settings."
    MessageBox MB_OK|MB_ICONEXCLAMATION "Fetchpath is installed, but the optional video and audio tools could not be set up. You can retry from Settings."
    fetchpath_media_done:

    ; Chrome and Edge require the person to add an unpacked extension in the
    ; browser. Opening this folder only starts those steps; it grants no browser
    ; permission and automatic capture remains off until enabled in the popup.
    ${If} ${SectionIsSelected} ${SecBrowser}
      MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Show how to add the optional Fetchpath browser extension to Chrome or Edge?$\r$\n$\r$\nYour browser will ask you to load it. You can also find these steps later in Fetchpath Settings." IDNO fetchpath_browser_done
      ExecShell "open" "$INSTDIR\browser-extension"
      ${If} ${Errors}
        DetailPrint "Could not open the browser extension folder. Open it from Fetchpath Settings later."
      ${EndIf}
      MessageBox MB_OK|MB_ICONINFORMATION "In Chrome, open chrome://extensions; in Edge, open edge://extensions. Turn on Developer mode, choose Load unpacked, then select the browser-extension folder that Setup opened. To check the connection later, open Fetchpath Settings > Browser extension."
    ${EndIf}
    fetchpath_browser_done:
  ${EndIf}
!macroend

; Decides, once the engine is stopped, whether the data is to be removed.
; Nothing is deleted here: the app window may still be open until the bundler
; has closed it, and the script is copied out because $INSTDIR is emptied
; before POSTUNINSTALL.
!macro FETCHPATH_PLAN_DATA_REMOVAL
  Push $R0
  StrCpy $FetchpathWipeData 0
  Delete "$TEMP\fetchpath-uninstall.log"
  !insertmacro FETCHPATH_LOG "plan: cmdline=[$CMDLINE] checkbox=$DeleteAppDataCheckboxState update=$UpdateMode"
  ClearErrors
  ${GetOptions} $CMDLINE "/DELETEAPPDATA" $R0
  ${IfNot} ${Errors}
    StrCpy $DeleteAppDataCheckboxState 1
  ${EndIf}
  !insertmacro FETCHPATH_LOG "plan: after switch checkbox=$DeleteAppDataCheckboxState"
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    InitPluginsDir
    ClearErrors
    CopyFiles /SILENT "$INSTDIR\tools\remove-app-data.ps1" "$PLUGINSDIR\remove-app-data.ps1"
    ${If} ${Errors}
      DetailPrint "Could not prepare the data removal. Your Fetchpath data was kept."
      MessageBox MB_OK|MB_ICONEXCLAMATION "Fetchpath could not prepare removing its data, so the data was kept. Delete the app.fetchpath.desktop folders under %APPDATA% and %LOCALAPPDATA% yourself if you want them gone." /SD IDOK
      SetErrorLevel 3
    ${Else}
      StrCpy $FetchpathWipeData 1
    ${EndIf}
    ; Either way the bundler's own RmDir /r must not run on these folders.
    StrCpy $DeleteAppDataCheckboxState 0
  ${EndIf}
  !insertmacro FETCHPATH_LOG "plan: wipe=$FetchpathWipeData"
  Pop $R0
!macroend

!macro FETCHPATH_REMOVE_DATA
  !insertmacro FETCHPATH_LOG "remove: wipe=$FetchpathWipeData"
  ${If} $FetchpathWipeData = 1
    DetailPrint "Removing the Fetchpath application data."
    nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\remove-app-data.ps1" -Roaming "$APPDATA" -Local "$LOCALAPPDATA"'
    Pop $R9
    !insertmacro FETCHPATH_LOG "remove: script exit=$R9"
    ${If} $R9 != 0
      DetailPrint "Some Fetchpath data could not be removed (exit $R9). It is listed above."
      MessageBox MB_OK|MB_ICONEXCLAMATION "Fetchpath was removed, but some of its data could not be deleted. Setup's log lists what is left; delete the app.fetchpath.desktop folders under %APPDATA% and %LOCALAPPDATA% yourself." /SD IDOK
      SetErrorLevel 3
    ${EndIf}
    ; The bundler's other cleanup for this choice.
    DeleteRegKey SHCTX "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty SHCTX "${MANUKEY}"
    DeleteRegValue HKCU "${MANUPRODUCTKEY}" "Installer Language"
    DeleteRegKey /ifempty HKCU "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty HKCU "${MANUKEY}"
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Before the files go, while fetchpath.exe and tools\user-path.ps1 are
  ; still installed.
  !insertmacro FETCHPATH_STOP_ENGINE
  !insertmacro FETCHPATH_USER_PATH Remove
  !insertmacro FETCHPATH_PLAN_DATA_REMOVAL
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  !insertmacro FETCHPATH_REMOVE_HOST_KEYS
  !insertmacro FETCHPATH_REMOVE_SIGN_IN
  !insertmacro FETCHPATH_REMOVE_DATA
  Delete "${FETCHPATH_UPDATE_HOLD}"
!macroend
