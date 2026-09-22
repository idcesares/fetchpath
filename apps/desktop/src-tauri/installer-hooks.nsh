; Fetchpath installer hooks.
;
; Referenced from tauri.conf.json as `bundle.windows.nsis.installerHooks`.
;
; This file exists for one reason: FP-017 shipped an uninstaller that removed
; the program files and left every byte of user data behind, with no way for the
; user to say otherwise. That was recorded honestly at the time rather than
; described as removal, and this closes it.
;
; The rule the branch follows is that the user decides, and the uninstaller then
; removes exactly what they chose and nothing else:
;
;   - Answering **No** (the default) leaves the queue, settings, browser inbox
;     and downloaded media helpers exactly where they are. An uninstall that is
;     really a reinstall must not lose a half-finished download.
;   - Answering **Yes** removes both directories Fetchpath created for its own
;     data. Downloaded *files* are never touched, wherever the user saved them:
;     those are the user's documents, not Fetchpath's data, and a download
;     manager that deletes your downloads when you uninstall it is a data-loss
;     bug, not a thorough cleanup.
;
; A silent uninstall keeps the data, because there is nobody present to choose
; and keeping is the recoverable answer.
;
; The two locations are the ones `tests/compatibility/windows/packaging-lifecycle.ps1`
; observes on a real machine, not a guess about where Tauri puts things:
;
;   %APPDATA%\app.fetchpath.desktop        queue-v1.json, settings-v1.json,
;                                          the browser inbox, media-tools\
;   %LOCALAPPDATA%\app.fetchpath.desktop   the WebView2 profile
;
; The identifier is written out rather than taken from a macro so that a change
; to `tauri.conf.json` `identifier` fails the packaging test loudly instead of
; silently pointing this branch at a directory that does not exist.

!macro NSIS_HOOK_PREINSTALL
!macroend

!macro NSIS_HOOK_POSTINSTALL
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  StrCpy $R0 "$APPDATA\app.fetchpath.desktop"
  StrCpy $R1 "$LOCALAPPDATA\app.fetchpath.desktop"

  ; Nothing to ask about when neither directory exists.
  IfFileExists "$R0\*.*" fetchpath_ask 0
  IfFileExists "$R1\*.*" fetchpath_ask 0
  Goto fetchpath_uninstall_done

fetchpath_ask:
  ; A silent uninstall has nobody to answer, so it takes the keeping branch.
  IfSilent fetchpath_keep_data

  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 \
    "Also remove your Fetchpath download queue, history and settings?$\r$\n$\r$\n\
     Choose No to keep them, which is what you want if you are reinstalling.$\r$\n$\r$\n\
     Files you have already downloaded are never removed, wherever you saved them." \
    /SD IDNO IDYES fetchpath_remove_data IDNO fetchpath_keep_data

fetchpath_remove_data:
  ; Only the two directories Fetchpath created. Never a destination folder, and
  ; never the Windows Downloads folder.
  RMDir /r "$R0"
  RMDir /r "$R1"
  DetailPrint "Removed Fetchpath application data."
  Goto fetchpath_uninstall_done

fetchpath_keep_data:
  DetailPrint "Kept Fetchpath application data in $R0"

fetchpath_uninstall_done:
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
!macroend
