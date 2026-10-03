; Fetchpath NSIS template (FP-099).
;
; Forked from the tauri-bundler NSIS template shipped with @tauri-apps/cli
; 2.11.4 (sha256 of the upstream template, line endings normalised to LF:
; 20f4ecc730defb71f1342eaeaec4021df13be3d843abba0effe88ea5835fa079).
; tests/installer/template-fork.test.mjs extracts the template from the
; installed CLI and fails when it differs, so a Tauri upgrade forces a reviewed
; re-merge. The design is docs/architecture/specs/2026-10-02-installer-components-design.md.
; Everything below that is not Fetchpath's is upstream; Fetchpath's changes are
; the "FP-099" blocks: component sections, the installation type and component
; pages, quiet /COMPONENTS, stored selections and removal on deselection.
Unicode true
ManifestDPIAware true
; Add in `dpiAwareness` `PerMonitorV2` to manifest for Windows 10 1607+ (note this should not affect lower versions since they should be able to ignore this and pick up `dpiAware` `true` set by `ManifestDPIAware true`)
; Currently undocumented on NSIS's website but is in the Docs folder of source tree, see
; https://github.com/kichik/nsis/blob/5fc0b87b819a9eec006df4967d08e522ddd651c9/Docs/src/attributes.but#L286-L300
; https://github.com/tauri-apps/tauri/pull/10106
ManifestDPIAwareness PerMonitorV2

!if "{{compression}}" == "none"
  SetCompress off
!else
  ; Set the compression algorithm. We default to LZMA.
  SetCompressor /SOLID "{{compression}}"
!endif

; Keep above !include to stay ahead of any plugin command
; see https://github.com/tauri-apps/tauri/pull/15422#discussion_r3289239624
{{#if signed_plugins_path}}
!addplugindir "{{signed_plugins_path}}"
{{/if}}

!include MUI2.nsh
!include FileFunc.nsh
!include x64.nsh
!include WordFunc.nsh
!include Sections.nsh
!include "utils.nsh"
!include "FileAssociation.nsh"
!include "Win\COM.nsh"
!include "Win\Propkey.nsh"
!include "StrFunc.nsh"
${StrCase}
${StrLoc}

{{#if installer_hooks}}
!include "{{installer_hooks}}"
{{/if}}

!define WEBVIEW2APPGUID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"

!define MANUFACTURER "{{manufacturer}}"
!define PRODUCTNAME "{{product_name}}"
!define VERSION "{{version}}"
!define VERSIONWITHBUILD "{{version_with_build}}"
!define HOMEPAGE "{{homepage}}"
!define INSTALLMODE "{{install_mode}}"
!define LICENSE "{{license}}"
!define INSTALLERICON "{{installer_icon}}"
!define SIDEBARIMAGE "{{sidebar_image}}"
!define HEADERIMAGE "{{header_image}}"
!define UNINSTALLERICON "{{uninstaller_icon}}"
!define UNINSTALLERHEADERIMAGE "{{uninstaller_header_image}}"
!define MAINBINARYNAME "{{main_binary_name}}"
!define MAINBINARYSRCPATH "{{main_binary_path}}"
!define BUNDLEID "{{bundle_id}}"
!define COPYRIGHT "{{copyright}}"
!define OUTFILE "{{out_file}}"
!define ARCH "{{arch}}"
!define ADDITIONALPLUGINSPATH "{{additional_plugins_path}}"
!define ALLOWDOWNGRADES "{{allow_downgrades}}"
!define DISPLAYLANGUAGESELECTOR "{{display_language_selector}}"
!define INSTALLWEBVIEW2MODE "{{install_webview2_mode}}"
!define WEBVIEW2INSTALLERARGS "{{webview2_installer_args}}"
!define WEBVIEW2BOOTSTRAPPERPATH "{{webview2_bootstrapper_path}}"
!define WEBVIEW2INSTALLERPATH "{{webview2_installer_path}}"
!define MINIMUMWEBVIEW2VERSION "{{minimum_webview2_version}}"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCTNAME}"
!define MANUKEY "Software\${MANUFACTURER}"
!define MANUPRODUCTKEY "${MANUKEY}\${PRODUCTNAME}"
!define UNINSTALLERSIGNCOMMAND "{{uninstaller_sign_cmd}}"
!define ESTIMATEDSIZE "{{estimated_size}}"
!define STARTMENUFOLDER "{{start_menu_folder}}"

Var PassiveMode
Var UpdateMode
Var NoShortcutMode
Var WixMode
Var OldMainBinaryName

; FP-099. Selection state. The sections' own check states are the truth during
; setup; these hold what was there before, the choices of the two pages and the
; reason a quiet selection was refused.
Var FpHasOld        ; 1 when an install exists (any version)
Var FpOldType       ; full | custom, as stored or derived
Var FpOldDesktop    ; the previous selection, 0 or 1 each
Var FpOldCli
Var FpOldMcp
Var FpOldBrowser
Var FpOldTorrent
Var FpType          ; full | custom, written after install
Var FpMedia         ; 1 when the person ticked the media tools box
Var FpSkipSelect    ; 1 to skip the type and component pages (repair)
Var FpReinstallChoice ; same-version choice: 1 change, 2 repair, 3 uninstall
Var FpVerCmp        ; result of the version comparison on the reinstall page
Var FpRbChange
Var FpRbRepair
Var FpRbUninstall
Var FpRbFull
Var FpRbCustom
Var FpCbMedia
Var FpList          ; a component list being read or written
Var FpTok
Var FpIdx
Var FpCount
Var FpRest
Var FpDesktopOn      ; 1 when the Desktop component is selected (set by FpCheckDesktop)
Var FpReason
Var FpSelDesktop     ; the selection being decided, 0 or 1 each
Var FpSelCli
Var FpSelMcp
Var FpSelBrowser
Var FpSelTorrent

Name "${PRODUCTNAME}"
BrandingText "${COPYRIGHT}"
OutFile "${OUTFILE}"

; We don't actually use this value as default install path,
; it's just for nsis to append the product name folder in the directory selector
; https://nsis.sourceforge.io/Reference/InstallDir
!define PLACEHOLDER_INSTALL_DIR "placeholder\${PRODUCTNAME}"
InstallDir "${PLACEHOLDER_INSTALL_DIR}"

VIProductVersion "${VERSIONWITHBUILD}"
VIAddVersionKey "ProductName" "${PRODUCTNAME}"
VIAddVersionKey "FileDescription" "${PRODUCTNAME}"
VIAddVersionKey "LegalCopyright" "${COPYRIGHT}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

# additional plugins
!addplugindir "${ADDITIONALPLUGINSPATH}"

; Uninstaller signing command
!if "${UNINSTALLERSIGNCOMMAND}" != ""
  !uninstfinalize '${UNINSTALLERSIGNCOMMAND}'
!endif

; Handle install mode, `perUser`, `perMachine` or `both`
!if "${INSTALLMODE}" == "perMachine"
  RequestExecutionLevel admin
!endif

!if "${INSTALLMODE}" == "currentUser"
  RequestExecutionLevel user
!endif

!if "${INSTALLMODE}" == "both"
  !define MULTIUSER_MUI
  !define MULTIUSER_INSTALLMODE_INSTDIR "${PRODUCTNAME}"
  !define MULTIUSER_INSTALLMODE_COMMANDLINE
  !if "${ARCH}" == "x64"
    !define MULTIUSER_USE_PROGRAMFILES64
  !else if "${ARCH}" == "arm64"
    !define MULTIUSER_USE_PROGRAMFILES64
  !endif
  !define MULTIUSER_INSTALLMODE_DEFAULT_REGISTRY_KEY "${UNINSTKEY}"
  !define MULTIUSER_INSTALLMODE_DEFAULT_REGISTRY_VALUENAME "CurrentUser"
  !define MULTIUSER_INSTALLMODEPAGE_SHOWUSERNAME
  !define MULTIUSER_INSTALLMODE_FUNCTION RestorePreviousInstallLocation
  !define MULTIUSER_EXECUTIONLEVEL Highest
  !include MultiUser.nsh
!endif

; Installer icon
!if "${INSTALLERICON}" != ""
  !define MUI_ICON "${INSTALLERICON}"
!endif

; Installer sidebar image
!if "${SIDEBARIMAGE}" != ""
  !define MUI_WELCOMEFINISHPAGE_BITMAP "${SIDEBARIMAGE}"
!endif

; Enable header images for installer and uninstaller pages when either image is configured.
!if "${HEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE
!else if "${UNINSTALLERHEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE
!endif

; Installer header image
!if "${HEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE_BITMAP "${HEADERIMAGE}"
!endif

; Uninstaller header image
!if "${UNINSTALLERHEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE_UNBITMAP "${UNINSTALLERHEADERIMAGE}"
!endif

; Uninstaller icon
!if "${UNINSTALLERICON}" != ""
  !define MUI_UNICON "${UNINSTALLERICON}"
!endif

; Define registry key to store installer language
!define MUI_LANGDLL_REGISTRY_ROOT "HKCU"
!define MUI_LANGDLL_REGISTRY_KEY "${MANUPRODUCTKEY}"
!define MUI_LANGDLL_REGISTRY_VALUENAME "Installer Language"

; Installer pages, must be ordered as they appear
; 1. Welcome Page
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!insertmacro MUI_PAGE_WELCOME

; 2. License Page (if defined)
!if "${LICENSE}" != ""
  !define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
  !insertmacro MUI_PAGE_LICENSE "${LICENSE}"
!endif

; 3. Install mode (if it is set to `both`)
!if "${INSTALLMODE}" == "both"
  !define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
  !insertmacro MULTIUSER_PAGE_INSTALLMODE
!endif

; 4. Custom page to ask user if he wants to reinstall/uninstall
;    only if a previous installation was detected
Var ReinstallPageCheck
Page custom PageReinstall PageLeaveReinstall
Function PageReinstall
  ; Uninstall previous WiX installation if exists.
  ;
  ; A WiX installer stores the installation info in registry
  ; using a UUID and so we have to loop through all keys under
  ; `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall`
  ; and check if `DisplayName` and `Publisher` keys match ${PRODUCTNAME} and ${MANUFACTURER}
  ;
  ; This has a potential issue that there maybe another installation that matches
  ; our ${PRODUCTNAME} and ${MANUFACTURER} but wasn't installed by our WiX installer,
  ; however, this should be fine since the user will have to confirm the uninstallation
  ; and they can chose to abort it if doesn't make sense.
  StrCpy $0 0
  wix_loop:
    EnumRegKey $1 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall" $0
    StrCmp $1 "" wix_loop_done ; Exit loop if there is no more keys to loop on
    IntOp $0 $0 + 1
    ReadRegStr $R0 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1" "DisplayName"
    ReadRegStr $R1 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1" "Publisher"
    StrCmp "$R0$R1" "${PRODUCTNAME}${MANUFACTURER}" 0 wix_loop
    ReadRegStr $R0 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1" "UninstallString"
    ${StrCase} $R1 $R0 "L"
    ${StrLoc} $R0 $R1 "msiexec" ">"
    StrCmp $R0 0 0 wix_loop_done
    StrCpy $WixMode 1
    StrCpy $R6 "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1"
    Goto compare_version
  wix_loop_done:

  ; Check if there is an existing installation, if not, abort the reinstall page
  ReadRegStr $R0 SHCTX "${UNINSTKEY}" ""
  ReadRegStr $R1 SHCTX "${UNINSTKEY}" "UninstallString"
  ${IfThen} "$R0$R1" == "" ${|} Abort ${|}

  ; Compare this installar version with the existing installation
  ; and modify the messages presented to the user accordingly
  compare_version:
  StrCpy $R4 "$(older)"
  ${If} $WixMode = 1
    ReadRegStr $R0 HKLM "$R6" "DisplayVersion"
  ${Else}
    ReadRegStr $R0 SHCTX "${UNINSTKEY}" "DisplayVersion"
  ${EndIf}
  ${IfThen} $R0 == "" ${|} StrCpy $R4 "$(unknown)" ${|}

  nsis_tauri_utils::SemverCompare "${VERSION}" $R0
  Pop $R0
  StrCpy $FpVerCmp $R0
  ; Reinstalling the same version
  ${If} $R0 = 0
    StrCpy $R1 "$(alreadyInstalledLong)"
    StrCpy $R2 "$(addOrReinstall)"
    StrCpy $R3 "$(uninstallApp)"
    !insertmacro MUI_HEADER_TEXT "$(alreadyInstalled)" "$(chooseMaintenanceOption)"
  ; Upgrading
  ${ElseIf} $R0 = 1
    StrCpy $R1 "$(olderOrUnknownVersionInstalled)"
    StrCpy $R2 "$(uninstallBeforeInstalling)"
    StrCpy $R3 "$(dontUninstall)"
    !insertmacro MUI_HEADER_TEXT "$(alreadyInstalled)" "$(choowHowToInstall)"
  ; Downgrading
  ${ElseIf} $R0 = -1
    StrCpy $R1 "$(newerVersionInstalled)"
    StrCpy $R2 "$(uninstallBeforeInstalling)"
    !if "${ALLOWDOWNGRADES}" == "true"
      StrCpy $R3 "$(dontUninstall)"
    !else
      StrCpy $R3 "$(dontUninstallDowngrade)"
    !endif
    !insertmacro MUI_HEADER_TEXT "$(alreadyInstalled)" "$(choowHowToInstall)"
  ${Else}
    Abort
  ${EndIf}

  ; Skip showing the page if passive
  ;
  ; Note that we don't call this earlier at the begining
  ; of this function because we need to populate some variables
  ; related to current installed version if detected and whether
  ; we are downgrading or not.
  ${If} $PassiveMode = 1
    Call PageLeaveReinstall
  ${Else}
    nsDialogs::Create 1018
    Pop $R4
    ${IfThen} $(^RTL) = 1 ${|} nsDialogs::SetRTL $(^RTL) ${|}

    ${NSD_CreateLabel} 0 0 100% 24u $R1
    Pop $R1

    ; FP-099: the same version offers three choices. Change components is
    ; preselected; Repair reinstalls the stored components without asking again.
    ${If} $FpVerCmp = 0
      ${NSD_CreateFirstRadioButton} 30u 46u -30u 8u "Change components"
      Pop $FpRbChange
      ${NSD_OnClick} $FpRbChange FpReinstallSelect
      ${NSD_CreateRadioButton} 30u 62u -30u 8u "Repair: reinstall the components already installed"
      Pop $FpRbRepair
      ${NSD_OnClick} $FpRbRepair FpReinstallSelect
      ${NSD_CreateRadioButton} 30u 78u -30u 8u "$(uninstallApp)"
      Pop $FpRbUninstall
      ${NSD_OnClick} $FpRbUninstall FpReinstallSelect
      ${If} $FpReinstallChoice = 2
        SendMessage $FpRbRepair ${BM_SETCHECK} ${BST_CHECKED} 0
      ${ElseIf} $FpReinstallChoice = 3
        SendMessage $FpRbUninstall ${BM_SETCHECK} ${BST_CHECKED} 0
      ${Else}
        SendMessage $FpRbChange ${BM_SETCHECK} ${BST_CHECKED} 0
      ${EndIf}
      ${NSD_SetFocus} $FpRbChange
    ${Else}
    ${NSD_CreateRadioButton} 30u 50u -30u 8u $R2
    Pop $R2
    ${NSD_OnClick} $R2 PageReinstallUpdateSelection

    ${NSD_CreateRadioButton} 30u 70u -30u 8u $R3
    Pop $R3
    ; Disable this radio button if downgrading and downgrades are disabled
    !if "${ALLOWDOWNGRADES}" == "false"
      ${IfThen} $R0 = -1 ${|} EnableWindow $R3 0 ${|}
    !endif
    ${NSD_OnClick} $R3 PageReinstallUpdateSelection

    ; Check the first radio button if this the first time
    ; we enter this page or if the second button wasn't
    ; selected the last time we were on this page
    ${If} $ReinstallPageCheck <> 2
      SendMessage $R2 ${BM_SETCHECK} ${BST_CHECKED} 0
    ${Else}
      SendMessage $R3 ${BM_SETCHECK} ${BST_CHECKED} 0
    ${EndIf}

    ${NSD_SetFocus} $R2
    ${EndIf}
    nsDialogs::Show
  ${EndIf}
FunctionEnd
Function FpReinstallSelect
  ${NSD_GetState} $FpRbRepair $R1
  ${If} $R1 == ${BST_CHECKED}
    StrCpy $FpReinstallChoice 2
    Return
  ${EndIf}
  ${NSD_GetState} $FpRbUninstall $R1
  ${If} $R1 == ${BST_CHECKED}
    StrCpy $FpReinstallChoice 3
  ${Else}
    StrCpy $FpReinstallChoice 1
  ${EndIf}
FunctionEnd
Function PageReinstallUpdateSelection
  ${NSD_GetState} $R2 $R1
  ${If} $R1 == ${BST_CHECKED}
    StrCpy $ReinstallPageCheck 1
  ${Else}
    StrCpy $ReinstallPageCheck 2
  ${EndIf}
FunctionEnd
Function PageLeaveReinstall
  ${NSD_GetState} $R2 $R1

  ; If migrating from Wix, always uninstall
  ${If} $WixMode = 1
    Goto reinst_uninstall
  ${EndIf}

  ; In update mode, always proceeds without uninstalling
  ${If} $UpdateMode = 1
    Goto reinst_done
  ${EndIf}

  ; $R0 holds whether same(0)/upgrading(1)/downgrading(-1) version
  ; $R1 holds the radio buttons state:
  ;   1 => first choice was selected
  ;   0 => second choice was selected
  ${If} $FpVerCmp = 0 ; Same version, proceed
    StrCpy $FpSkipSelect 0 ; Back from Repair, then Change, must show the pages
    ${If} $FpReinstallChoice = 3 ; User chose to uninstall
      Goto reinst_uninstall
    ${EndIf}
    ${If} $FpReinstallChoice = 2 ; Repair keeps the stored components
      StrCpy $FpSkipSelect 1
    ${EndIf}
    Goto reinst_done
  ${ElseIf} $FpVerCmp = 1 ; Upgrading
    ${If} $R1 = 1              ; User chose to uninstall
      Goto reinst_uninstall
    ${Else}
      Goto reinst_done         ; User chose NOT to uninstall
    ${EndIf}
  ${ElseIf} $FpVerCmp = -1 ; Downgrading
    ${If} $R1 = 1              ; User chose to uninstall
      Goto reinst_uninstall
    ${Else}
      Goto reinst_done         ; User chose NOT to uninstall
    ${EndIf}
  ${EndIf}

  reinst_uninstall:
    HideWindow
    ClearErrors

    ${If} $WixMode = 1
      ReadRegStr $R1 HKLM "$R6" "UninstallString"
      ExecWait '$R1' $0
    ${Else}
      ReadRegStr $4 SHCTX "${MANUPRODUCTKEY}" ""
      ReadRegStr $R1 SHCTX "${UNINSTKEY}" "UninstallString"
      ${IfThen} $UpdateMode = 1 ${|} StrCpy $R1 "$R1 /UPDATE" ${|} ; append /UPDATE
      ${IfThen} $PassiveMode = 1 ${|} StrCpy $R1 "$R1 /P" ${|} ; append /P
      StrCpy $R1 "$R1 _?=$4" ; append uninstall directory
      ExecWait '$R1' $0
    ${EndIf}

    BringToFront

    ${IfThen} ${Errors} ${|} StrCpy $0 2 ${|} ; ExecWait failed, set fake exit code

    ${If} $0 <> 0
    ${OrIf} ${FileExists} "$INSTDIR\fetchpath.exe" ; FP-099: Core, not the desktop exe
      ; User cancelled wix uninstaller? return to select un/reinstall page
      ${If} $WixMode = 1
      ${AndIf} $0 = 1602
        Abort
      ${EndIf}

      ; User cancelled NSIS uninstaller? return to select un/reinstall page
      ${If} $0 = 1
        Abort
      ${EndIf}

      ; Other erros? show generic error message and return to select un/reinstall page
      MessageBox MB_ICONEXCLAMATION "$(unableToUninstall)"
      Abort
    ${EndIf}
  reinst_done:
FunctionEnd

; FP-099: 4a. Installation type. Interactive setups only: a quiet or passive
; setup takes its selection from the stored values or /COMPONENTS in .onInit,
; and a Repair choice on the page above keeps the stored components.
!ifndef BS_MULTILINE
  !define BS_MULTILINE 0x2000
!endif
Page custom FpTypePage FpTypeLeave
Function FpTypePage
  ${IfThen} $PassiveMode = 1 ${|} Abort ${|}
  ${IfThen} $FpSkipSelect = 1 ${|} Abort ${|}
  !insertmacro MUI_HEADER_TEXT "Installation type" "Choose what to install."
  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}
  ${NSD_CreateFirstRadioButton} 0 6u 100% 24u "Full (recommended): the app, the terminal command, AI agent support, browser integration and the torrent helper. About 37 MiB."
  Pop $FpRbFull
  ${NSD_AddStyle} $FpRbFull ${BS_MULTILINE}
  ${NSD_CreateRadioButton} 0 36u 100% 12u "Custom: choose what to install."
  Pop $FpRbCustom
  ${NSD_CreateCheckBox} 0 66u 100% 10u "Also download video and audio tools (yt-dlp, FFmpeg) from their publishers now."
  Pop $FpCbMedia
  ${NSD_CreateLabel} 12u 78u -12u 24u "Third-party licences apply, and Fetchpath checks their pinned SHA-256 values. You can do this later in Settings."
  Pop $0
  ${If} $FpType == "custom"
    SendMessage $FpRbCustom ${BM_SETCHECK} ${BST_CHECKED} 0
    ${NSD_SetFocus} $FpRbCustom
  ${Else}
    SendMessage $FpRbFull ${BM_SETCHECK} ${BST_CHECKED} 0
    ${NSD_SetFocus} $FpRbFull
  ${EndIf}
  nsDialogs::Show
FunctionEnd
Function FpTypeLeave
  ${NSD_GetState} $FpRbFull $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $FpType full
    Call FpSelectAll
  ${Else}
    StrCpy $FpType custom
  ${EndIf}
  ${NSD_GetState} $FpCbMedia $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $FpMedia 1
  ${Else}
    StrCpy $FpMedia 0
  ${EndIf}
FunctionEnd

; FP-099: 4b. Components (Custom only).
Function FpComponentsPre
  ${IfThen} $PassiveMode = 1 ${|} Abort ${|}
  ${IfThen} $FpSkipSelect = 1 ${|} Abort ${|}
  ${IfThen} $FpType == "full" ${|} Abort ${|}
FunctionEnd
!define MUI_COMPONENTSPAGE_SMALLDESC
!define MUI_PAGE_HEADER_TEXT "Choose components"
!define MUI_PAGE_HEADER_SUBTEXT "Choose how you use Fetchpath and which optional modules to install."
!define MUI_COMPONENTSPAGE_TEXT_TOP "Core is always installed. Choose the app or the terminal, then any modules you want. Installing a component never gives an AI agent access and never turns on sharing."
!define MUI_PAGE_CUSTOMFUNCTION_PRE FpComponentsPre
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE FpComponentsLeave
!insertmacro MUI_PAGE_COMPONENTS

; 5. Choose install directory page
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!insertmacro MUI_PAGE_DIRECTORY

; 6. Start menu shortcut page
Var AppStartMenuFolder
!if "${STARTMENUFOLDER}" != ""
  !define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
  !define MUI_STARTMENUPAGE_DEFAULTFOLDER "${STARTMENUFOLDER}"
!else
  !define MUI_PAGE_CUSTOMFUNCTION_PRE Skip
!endif
!insertmacro MUI_PAGE_STARTMENU Application $AppStartMenuFolder

; 7. Installation page
!insertmacro MUI_PAGE_INSTFILES

; 8. Finish page
;
; Don't auto jump to finish page after installation page,
; because the installation page has useful info that can be used debug any issues with the installer.
!define MUI_FINISHPAGE_NOAUTOCLOSE
; Use show readme button in the finish page as a button create a desktop shortcut
!define MUI_FINISHPAGE_SHOWREADME
!define MUI_FINISHPAGE_SHOWREADME_TEXT "$(createDesktop)"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateOrUpdateDesktopShortcut
; Show run app after installation.
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_FUNCTION RunMainBinary
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!define MUI_PAGE_CUSTOMFUNCTION_SHOW FpFinishShow
!insertmacro MUI_PAGE_FINISH

Function RunMainBinary
  ${If} $FpDesktopOn = 1
    nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" ""
  ${EndIf}
FunctionEnd

; Uninstaller Pages
; 1. Confirm uninstall page
Var DeleteAppDataCheckbox
Var DeleteAppDataCheckboxState
!define /ifndef WS_EX_LAYOUTRTL         0x00400000
!define MUI_PAGE_CUSTOMFUNCTION_SHOW un.ConfirmShow
Function un.ConfirmShow ; Add add a `Delete app data` check box
  ; $1 inner dialog HWND
  ; $2 window DPI
  ; $3 style
  ; $4 x
  ; $5 y
  ; $6 width
  ; $7 height
  FindWindow $1 "#32770" "" $HWNDPARENT ; Find inner dialog
  System::Call "user32::GetDpiForWindow(p r1) i .r2"
  ${If} $(^RTL) = 1
    StrCpy $3 "${__NSD_CheckBox_EXSTYLE} | ${WS_EX_LAYOUTRTL}"
    IntOp $4 50 * $2
  ${Else}
    StrCpy $3 "${__NSD_CheckBox_EXSTYLE}"
    IntOp $4 0 * $2
  ${EndIf}
  IntOp $5 100 * $2
  IntOp $6 400 * $2
  IntOp $7 25 * $2
  IntOp $4 $4 / 96
  IntOp $5 $5 / 96
  IntOp $6 $6 / 96
  IntOp $7 $7 / 96
  System::Call 'user32::CreateWindowEx(i r3, w "${__NSD_CheckBox_CLASS}", w "$(deleteAppData)", i ${__NSD_CheckBox_STYLE}, i r4, i r5, i r6, i r7, p r1, i0, i0, i0) i .s'
  Pop $DeleteAppDataCheckbox
  SendMessage $HWNDPARENT ${WM_GETFONT} 0 0 $1
  SendMessage $DeleteAppDataCheckbox ${WM_SETFONT} $1 1
FunctionEnd
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE un.ConfirmLeave
Function un.ConfirmLeave
  SendMessage $DeleteAppDataCheckbox ${BM_GETCHECK} 0 0 $DeleteAppDataCheckboxState
FunctionEnd
!define MUI_PAGE_CUSTOMFUNCTION_PRE un.SkipIfPassive
!insertmacro MUI_UNPAGE_CONFIRM

; 2. Uninstalling Page
!insertmacro MUI_UNPAGE_INSTFILES

;Languages
{{#each languages}}
!insertmacro MUI_LANGUAGE "{{this}}"
{{/each}}
!insertmacro MUI_RESERVEFILE_LANGDLL
{{#each language_files}}
  !include "{{this}}"
{{/each}}

; FP-099: which component owns a bundled file. Core owns everything the
; Fetchpath components below do not name, so a file added to the bundle later
; is installed with Core rather than lost. Resolved at compile time.
!macro FETCHPATH_RESOURCE WANT ONAME SRC
  !define FP_RES_OWNER "core"
  !searchparse /noerrors "${ONAME}" "browser-extension\" FP_RES_REST
  !ifdef FP_RES_REST
    !undef FP_RES_REST
    !undef FP_RES_OWNER
    !define FP_RES_OWNER "browser"
  !endif
  !searchparse /noerrors "${ONAME}" "com.fetchpath.browser." FP_RES_REST
  !ifdef FP_RES_REST
    !undef FP_RES_REST
    !undef FP_RES_OWNER
    !define FP_RES_OWNER "browser"
  !endif
  !if "${FP_RES_OWNER}" == "${WANT}"
    File /a "/oname=${ONAME}" "${SRC}"
  !endif
  !undef FP_RES_OWNER
!macroend
!macro FETCHPATH_RESDIR WANT DIR
  !define FP_DIR_OWNER "core"
  !searchparse /noerrors "${DIR}" "browser-extension" FP_DIR_REST
  !ifdef FP_DIR_REST
    !undef FP_DIR_REST
    !undef FP_DIR_OWNER
    !define FP_DIR_OWNER "browser"
  !endif
  !if "${FP_DIR_OWNER}" == "${WANT}"
    CreateDirectory "$INSTDIR\${DIR}"
  !endif
  !undef FP_DIR_OWNER
!macroend
!macro FETCHPATH_BINARY WANT ONAME SRC
  !if "${ONAME}" == "fetchpath-browser-host.exe"
    !define FP_BIN_OWNER "browser"
  !else if "${ONAME}" == "fetchpath-torrent-helper.exe"
    !define FP_BIN_OWNER "torrent"
  !else
    !define FP_BIN_OWNER "core"
  !endif
  !if "${FP_BIN_OWNER}" == "${WANT}"
    File /a "/oname=${ONAME}" "${SRC}"
  !endif
  !undef FP_BIN_OWNER
!macroend
; The browser component's files, by name, for removal when it is deselected.
!macro FETCHPATH_RESOURCE_DELETE WANT ONAME
  !define FP_RES_OWNER "core"
  !searchparse /noerrors "${ONAME}" "browser-extension\" FP_RES_REST
  !ifdef FP_RES_REST
    !undef FP_RES_REST
    !undef FP_RES_OWNER
    !define FP_RES_OWNER "browser"
  !endif
  !searchparse /noerrors "${ONAME}" "com.fetchpath.browser." FP_RES_REST
  !ifdef FP_RES_REST
    !undef FP_RES_REST
    !undef FP_RES_OWNER
    !define FP_RES_OWNER "browser"
  !endif
  !if "${FP_RES_OWNER}" == "${WANT}"
    Delete "$INSTDIR\${ONAME}"
  !endif
  !undef FP_RES_OWNER
!macroend

Section "-EarlyChecks"
  ; Abort silent installer if downgrades is disabled
  !if "${ALLOWDOWNGRADES}" == "false"
  ${If} ${Silent}
    ; If downgrading
    ${If} $R0 = -1
      System::Call 'kernel32::AttachConsole(i -1)i.r0'
      ${If} $0 <> 0
        System::Call 'kernel32::GetStdHandle(i -11)i.r0'
        System::call 'kernel32::SetConsoleTextAttribute(i r0, i 0x0004)' ; set red color
        FileWrite $0 "$(silentDowngrades)"
      ${EndIf}
      Abort
    ${EndIf}
  ${EndIf}
  !endif

SectionEnd

; FP-099: Core. Always installed: the engine, the CLI, TUI and MCP (one
; fetchpath.exe), the guides, licences and helper scripts, and the uninstaller.
Section "Core" SecCore
  SectionIn RO
  SetOutPath $INSTDIR

  !ifmacrodef NSIS_HOOK_PREINSTALL
    !insertmacro NSIS_HOOK_PREINSTALL
  !endif

  ; Everything running from the install folder is stopped by the hook above
  ; (the engine first, then anything else), so a program that is about to be
  ; replaced or removed is not running. What the new selection drops goes now.
  Call FpRemoveDeselected

  ; Copy resources
  {{#each resources_dirs}}
    !insertmacro FETCHPATH_RESDIR "core" "{{this}}"
  {{/each}}
  {{#each resources}}
    !insertmacro FETCHPATH_RESOURCE "core" "{{this.[1]}}" "{{no-escape @key}}"
  {{/each}}

  ; Copy external binaries
  {{#each binaries}}
    !insertmacro FETCHPATH_BINARY "core" "{{this}}" "{{no-escape @key}}"
  {{/each}}

  ; Create uninstaller
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Save $INSTDIR in registry for future installations
  WriteRegStr SHCTX "${MANUPRODUCTKEY}" "" $INSTDIR

  !if "${INSTALLMODE}" == "both"
    ; Save install mode to be selected by default for the next installation such as updating
    ; or when uninstalling
    WriteRegStr SHCTX "${UNINSTKEY}" $MultiUser.InstallMode 1
  !endif

  ; Remove an old main binary that is neither Core's fetchpath.exe nor the
  ; desktop app. Never during /UPDATE, and never the desktop app: whether it is
  ; installed is the selection's decision, made above.
  ReadRegStr $OldMainBinaryName SHCTX "${UNINSTKEY}" "MainBinaryName"
  ${If} $OldMainBinaryName != ""
  ${AndIf} $OldMainBinaryName != "fetchpath.exe"
  ${AndIf} $OldMainBinaryName != "${MAINBINARYNAME}.exe"
  ${AndIf} $UpdateMode <> 1
    Delete "$INSTDIR\$OldMainBinaryName"
  ${EndIf}

  ; Save the main binary for future updates. With components it is Core's
  ; fetchpath.exe: it is the one file every selection installs, and it carries
  ; the icon.
  WriteRegStr SHCTX "${UNINSTKEY}" "MainBinaryName" "fetchpath.exe"

  ; Registry information for add/remove programs
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayName" "${PRODUCTNAME}"
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayIcon" "$\"$INSTDIR\fetchpath.exe$\""
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr SHCTX "${UNINSTKEY}" "Publisher" "${MANUFACTURER}"
  WriteRegStr SHCTX "${UNINSTKEY}" "InstallLocation" "$\"$INSTDIR$\""
  WriteRegStr SHCTX "${UNINSTKEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegDWORD SHCTX "${UNINSTKEY}" "NoModify" "1"
  WriteRegDWORD SHCTX "${UNINSTKEY}" "NoRepair" "1"

  !if "${HOMEPAGE}" != ""
    WriteRegStr SHCTX "${UNINSTKEY}" "URLInfoAbout" "${HOMEPAGE}"
    WriteRegStr SHCTX "${UNINSTKEY}" "URLUpdateInfo" "${HOMEPAGE}"
    WriteRegStr SHCTX "${UNINSTKEY}" "HelpLink" "${HOMEPAGE}"
  !endif
SectionEnd

SectionGroup /e "Ways to use Fetchpath" SecGrpUse

; FP-099: Desktop app. The one component with a runtime prerequisite.
Section "Desktop app" SecDesktop
  SetOutPath $INSTDIR

  ; Check if Webview2 is already installed and skip this section
  ${If} ${RunningX64}
    ReadRegStr $4 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${Else}
    ReadRegStr $4 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}
  ${If} $4 == ""
    ReadRegStr $4 HKCU "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}

  ${If} $4 == ""
    ; Webview2 installation
    ;
    ; Skip if updating
    ${If} $UpdateMode <> 1
      !if "${INSTALLWEBVIEW2MODE}" == "downloadBootstrapper"
        Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        DetailPrint "$(webview2Downloading)"
        NSISdl::download "https://go.microsoft.com/fwlink/p/?LinkId=2124703" "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        Pop $0
        ${If} $0 == "success"
          DetailPrint "$(webview2DownloadSuccess)"
        ${Else}
          !insertmacro FETCHPATH_INSTALL_LOG "WebView2 bootstrapper download failed: $0"
          DetailPrint "$(webview2DownloadError)"
          Abort "$(webview2AbortError)"
        ${EndIf}
        StrCpy $6 "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        Goto install_webview2
      !endif

      !if "${INSTALLWEBVIEW2MODE}" == "embedBootstrapper"
        Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        File "/oname=$TEMP\MicrosoftEdgeWebview2Setup.exe" "${WEBVIEW2BOOTSTRAPPERPATH}"
        DetailPrint "$(installingWebview2)"
        StrCpy $6 "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        Goto install_webview2
      !endif

      !if "${INSTALLWEBVIEW2MODE}" == "offlineInstaller"
        Delete "$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe"
        File "/oname=$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe" "${WEBVIEW2INSTALLERPATH}"
        DetailPrint "$(installingWebview2)"
        StrCpy $6 "$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe"
        Goto install_webview2
      !endif

      Goto webview2_done

      install_webview2:
        DetailPrint "$(installingWebview2)"
        ; $6 holds the path to the webview2 installer
        ExecWait "$6 ${WEBVIEW2INSTALLERARGS} /install" $1
        !insertmacro FETCHPATH_INSTALL_LOG "WebView2 installer exit code $1"
        ${If} $1 = 0
          DetailPrint "$(webview2InstallSuccess)"
        ${Else}
          DetailPrint "$(webview2InstallError)"
          Abort "$(webview2AbortError)"
        ${EndIf}
      webview2_done:
    ${EndIf}
  ${Else}
    !if "${MINIMUMWEBVIEW2VERSION}" != ""
      ${VersionCompare} "${MINIMUMWEBVIEW2VERSION}" "$4" $R0
      ${If} $R0 = 1
        update_webview:
          DetailPrint "$(installingWebview2)"
          ${If} ${RunningX64}
            ReadRegStr $R1 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate" "path"
          ${Else}
            ReadRegStr $R1 HKLM "SOFTWARE\Microsoft\EdgeUpdate" "path"
          ${EndIf}
          ${If} $R1 == ""
            ReadRegStr $R1 HKCU "SOFTWARE\Microsoft\EdgeUpdate" "path"
          ${EndIf}
          ${If} $R1 != ""
            ; Chromium updater docs: https://source.chromium.org/chromium/chromium/src/+/main:docs/updater/user_manual.md
            ; Modified from "HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Microsoft EdgeWebView\ModifyPath"
            ExecWait `"$R1" /install appguid=${WEBVIEW2APPGUID}&needsadmin=true` $1
            ${If} $1 = 0
              DetailPrint "$(webview2InstallSuccess)"
            ${Else}
              MessageBox MB_ICONEXCLAMATION|MB_ABORTRETRYIGNORE "$(webview2InstallError)" IDIGNORE ignore IDRETRY update_webview
              Quit
              ignore:
            ${EndIf}
          ${EndIf}
      ${EndIf}
    !endif
  ${EndIf}

  ; Copy main executable
  File "${MAINBINARYSRCPATH}"

  ; Create file associations
  {{#each file_associations as |association| ~}}
    {{#each association.ext as |ext| ~}}
       !insertmacro APP_ASSOCIATE "{{ext}}" "{{or association.name ext}}" "{{association-description association.description ext}}" "$INSTDIR\${MAINBINARYNAME}.exe,0" "Open with ${PRODUCTNAME}" "$INSTDIR\${MAINBINARYNAME}.exe $\"%1$\""
    {{/each}}
  {{/each}}

  ; Register deep links
  {{#each deep_link_protocols as |protocol| ~}}
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}" "URL Protocol" ""
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}" "" "URL:${BUNDLEID} protocol"
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}\DefaultIcon" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}\shell\open\command" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
  {{/each}}
SectionEnd

; FP-099: Terminal and AI agents add entry points to files Core already
; installed (the PATH entry and the Start entry are made in the last section),
; so these two copy nothing.
Section "Terminal (CLI and TUI)" SecCli
  SetOutPath $INSTDIR
SectionEnd

Section "AI agents (MCP)" SecMcp
  SetOutPath $INSTDIR
SectionEnd

SectionGroupEnd

SectionGroup /e "Optional modules" SecGrpMods

Section "Browser integration" SecBrowser
  SetOutPath $INSTDIR
  {{#each resources_dirs}}
    !insertmacro FETCHPATH_RESDIR "browser" "{{this}}"
  {{/each}}
  {{#each resources}}
    !insertmacro FETCHPATH_RESOURCE "browser" "{{this.[1]}}" "{{no-escape @key}}"
  {{/each}}
  {{#each binaries}}
    !insertmacro FETCHPATH_BINARY "browser" "{{this}}" "{{no-escape @key}}"
  {{/each}}
SectionEnd

Section "Torrent helper" SecTorrent
  SetOutPath $INSTDIR
  {{#each binaries}}
    !insertmacro FETCHPATH_BINARY "torrent" "{{this}}" "{{no-escape @key}}"
  {{/each}}
SectionEnd

SectionGroupEnd

; FP-099: the registrations that depend on the whole selection, then the hooks'
; post-install step. Runs after every component section.
Section "-Finish" SecFinish
  SetOutPath $INSTDIR
  Call FpCheckDesktop

  ; Remember the selection with the uninstall entry; it goes when the entry goes.
  Call FpWriteSelection

  ${GetSize} "$INSTDIR" "/M=uninstall.exe /S=0K /G=0" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD SHCTX "${UNINSTKEY}" "EstimatedSize" "$0"

  ; Create start menu shortcut
  !insertmacro MUI_STARTMENU_WRITE_BEGIN Application
    Call FpUpdateStartMenuEntries
  !insertmacro MUI_STARTMENU_WRITE_END
  ; Outside the Start menu block, so a stale Terminal entry is removed even when
  ; shortcuts are not being created.
  Call FpUpdateTerminalEntry

  ; Create desktop shortcut for silent and passive installers
  ; because finish page will be skipped
  ${If} $PassiveMode = 1
  ${OrIf} ${Silent}
    ${If} ${SectionIsSelected} ${SecDesktop}
      Call CreateOrUpdateDesktopShortcut
    ${EndIf}
  ${EndIf}

  !ifmacrodef NSIS_HOOK_POSTINSTALL
    !insertmacro NSIS_HOOK_POSTINSTALL
  !endif

  ; Auto close this page for passive mode
  ${If} $PassiveMode = 1
    SetAutoClose true
  ${EndIf}
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore} "The engine, the fetchpath program (command line, terminal app and AI agent server in one file), the guides and licences. Always installed. About 10.8 MiB."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecGrpUse} "Choose at least one of the app or the terminal, so you can manage downloads and approve requests."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "The Fetchpath window, with Start menu and desktop shortcuts. About 13.3 MiB. Needs Microsoft WebView2, which Setup downloads from Microsoft if Windows does not have it."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCli} "The fetchpath command and the terminal app, added to your PATH. No extra disk space. Without the desktop app, Setup adds a Fetchpath Terminal entry to the Start menu."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecMcp} "Lets AI agent hosts start fetchpath mcp from your PATH. Agents get no access until you grant it. No extra disk space. Needs the app or the terminal, and adds the terminal if neither is chosen."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecGrpMods} "Extras you can add now or by running Setup again."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecBrowser} "Connects Chrome, Edge and Firefox to Fetchpath: the browser host (2.5 MiB), its registration for your user, and the extension folder. You still load the extension in your browser."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecTorrent} "Needed for torrent and magnet downloads. About 10.6 MiB. Installing it does not turn on seeding."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; --- selection helpers -------------------------------------------------------

Function FpCheckDesktop
  ${If} ${SectionIsSelected} ${SecDesktop}
    StrCpy $FpDesktopOn 1
  ${Else}
    StrCpy $FpDesktopOn 0
  ${EndIf}
FunctionEnd

Function FpSelectAll
  !insertmacro SelectSection ${SecDesktop}
  !insertmacro SelectSection ${SecCli}
  !insertmacro SelectSection ${SecMcp}
  !insertmacro SelectSection ${SecBrowser}
  !insertmacro SelectSection ${SecTorrent}
FunctionEnd

Function FpApplySelection
  ${If} $FpSelDesktop = 1
    !insertmacro SelectSection ${SecDesktop}
  ${Else}
    !insertmacro UnselectSection ${SecDesktop}
  ${EndIf}
  ${If} $FpSelCli = 1
    !insertmacro SelectSection ${SecCli}
  ${Else}
    !insertmacro UnselectSection ${SecCli}
  ${EndIf}
  ${If} $FpSelMcp = 1
    !insertmacro SelectSection ${SecMcp}
  ${Else}
    !insertmacro UnselectSection ${SecMcp}
  ${EndIf}
  ${If} $FpSelBrowser = 1
    !insertmacro SelectSection ${SecBrowser}
  ${Else}
    !insertmacro UnselectSection ${SecBrowser}
  ${EndIf}
  ${If} $FpSelTorrent = 1
    !insertmacro SelectSection ${SecTorrent}
  ${Else}
    !insertmacro UnselectSection ${SecTorrent}
  ${EndIf}
FunctionEnd

Function FpSelAllVars
  StrCpy $FpSelDesktop 1
  StrCpy $FpSelCli 1
  StrCpy $FpSelMcp 1
  StrCpy $FpSelBrowser 1
  StrCpy $FpSelTorrent 1
FunctionEnd

; Reads the comma-separated names in $FpList (lowercase) into $FpSel*. $FpReason
; is empty when the list is a valid selection, else says why it is not: an
; unknown or unavailable name, an empty entry, or no usable interface.
Function FpParseList
  StrCpy $FpReason ""
  StrCpy $FpSelDesktop 0
  StrCpy $FpSelCli 0
  StrCpy $FpSelMcp 0
  StrCpy $FpSelBrowser 0
  StrCpy $FpSelTorrent 0
  ${If} $FpList == ""
    StrCpy $FpReason "the component list is empty"
    Return
  ${EndIf}
  ; Entry by entry, up to each comma. WordFind is not used: it skips an empty
  ; entry and returns no count for a list without a comma.
  StrCpy $FpRest "$FpList"
  fp_parse_next:
    ${StrLoc} $FpIdx "$FpRest" "," ">"
    ${If} $FpIdx == ""
      StrCpy $FpTok "$FpRest"
      StrCpy $FpCount 1
    ${Else}
      StrCpy $FpTok "$FpRest" $FpIdx
      IntOp $FpIdx $FpIdx + 1
      StrCpy $FpRest "$FpRest" "" $FpIdx
      StrCpy $FpCount 0
    ${EndIf}
    ${If} $FpTok == ""
      StrCpy $FpReason "the component list has an empty entry"
      Return
    ${ElseIf} $FpTok == "core"
    ${ElseIf} $FpTok == "desktop"
      StrCpy $FpSelDesktop 1
    ${ElseIf} $FpTok == "cli"
      StrCpy $FpSelCli 1
    ${ElseIf} $FpTok == "mcp"
      StrCpy $FpSelMcp 1
    ${ElseIf} $FpTok == "browser"
      StrCpy $FpSelBrowser 1
    ${ElseIf} $FpTok == "torrent"
      StrCpy $FpSelTorrent 1
    ${ElseIf} $FpTok == "web"
      StrCpy $FpReason "web is not part of this build"
      Return
    ${Else}
      StrCpy $FpReason "unknown component [$FpTok]"
      Return
    ${EndIf}
    ${If} $FpCount = 0
      Goto fp_parse_next
    ${EndIf}
  ${If} $FpSelDesktop = 0
  ${AndIf} $FpSelCli = 0
    ${If} $FpSelMcp = 1
      StrCpy $FpReason "mcp needs desktop or cli"
    ${Else}
      StrCpy $FpReason "no usable interface: choose desktop or cli"
    ${EndIf}
  ${EndIf}
FunctionEnd

; What an install with no readable stored selection has on disk. The PATH entry
; is read by the same script that writes it; NSIS cannot read a long PATH.
Function FpDeriveFromDisk
  StrCpy $FpSelDesktop 0
  StrCpy $FpSelCli 0
  StrCpy $FpSelMcp 0
  StrCpy $FpSelBrowser 0
  StrCpy $FpSelTorrent 0
  ${If} ${FileExists} "$INSTDIR\${MAINBINARYNAME}.exe"
    StrCpy $FpSelDesktop 1
  ${EndIf}
  ${If} ${FileExists} "$INSTDIR\fetchpath-browser-host.exe"
    StrCpy $FpSelBrowser 1
  ${EndIf}
  ${If} ${FileExists} "$INSTDIR\fetchpath-torrent-helper.exe"
    StrCpy $FpSelTorrent 1
  ${EndIf}
  ${If} ${FileExists} "$INSTDIR\tools\user-path.ps1"
    nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$INSTDIR\tools\user-path.ps1" -Action Test -Dir "$INSTDIR"'
    Pop $0
    Pop $1
    ${If} $0 = 0
      StrCpy $FpSelCli 1
      StrCpy $FpSelMcp 1
    ${EndIf}
  ${EndIf}
  ${If} $FpSelDesktop = 0
  ${AndIf} $FpSelCli = 0
    StrCpy $FpSelCli 1
  ${EndIf}
FunctionEnd

; The previous install's selection into $FpSel*, $FpOldType, and a log line when
; the stored value could not be used. Never fatal.
Function FpReadStored
  ReadRegStr $FpOldType SHCTX "${UNINSTKEY}" "FetchpathInstallType"
  ReadRegStr $FpList SHCTX "${UNINSTKEY}" "FetchpathComponents"
  ClearErrors
  ReadRegDWORD $1 SHCTX "${UNINSTKEY}" "FetchpathComponentsSchema"
  ${If} ${Errors}
    StrCpy $1 0
  ${EndIf}
  StrCpy $FpReason ""
  ${If} $FpOldType == ""
  ${AndIf} $FpList == ""
    ; An install from before components existed has everything this build has.
    StrCpy $FpOldType "full"
    Call FpSelAllVars
    Return
  ${EndIf}
  ${StrCase} $FpOldType $FpOldType "L"
  ${StrCase} $FpList $FpList "L"
  ${If} $1 > 1
    StrCpy $FpReason "the stored components use a newer format ($1)"
  ${ElseIf} $FpOldType == "full"
    Call FpSelAllVars
    Return
  ${ElseIf} $FpOldType == "custom"
    Call FpParseList
  ${Else}
    StrCpy $FpReason "the stored install type is not full or custom"
  ${EndIf}
  ${If} $FpReason != ""
    !insertmacro FETCHPATH_INSTALL_LOG "stored selection unusable ($FpReason), reading the install folder"
    Call FpDeriveFromDisk
    StrCpy $FpOldType "custom"
  ${EndIf}
FunctionEnd

; A bad /COMPONENTS value ends setup before any file is touched.
Function FpRefuseSelection
  !insertmacro FETCHPATH_INSTALL_LOG "invalid component selection: $FpReason"
  ${IfNot} ${Silent}
  ${AndIf} $PassiveMode <> 1
    MessageBox MB_OK|MB_ICONSTOP "Setup was started with a component selection it cannot use: $FpReason."
  ${EndIf}
  SetErrorLevel 10
  Quit
FunctionEnd

; Decides the selection for this run: the stored one for an existing install, a
; /COMPONENTS value when given (not with /UPDATE), Full for a fresh machine.
Function FpInitSelection
  StrCpy $FpHasOld 0
  StrCpy $FpOldDesktop 0
  StrCpy $FpOldCli 0
  StrCpy $FpOldMcp 0
  StrCpy $FpOldBrowser 0
  StrCpy $FpOldTorrent 0
  StrCpy $FpType "full"
  StrCpy $FpSkipSelect 0
  StrCpy $FpMedia 0
  StrCpy $FpReinstallChoice 1
  Call FpSelAllVars

  ReadRegStr $0 SHCTX "${UNINSTKEY}" "UninstallString"
  ${If} $0 != ""
    StrCpy $FpHasOld 1
    Call FpReadStored
    StrCpy $FpType $FpOldType
    StrCpy $FpOldDesktop $FpSelDesktop
    StrCpy $FpOldCli $FpSelCli
    StrCpy $FpOldMcp $FpSelMcp
    StrCpy $FpOldBrowser $FpSelBrowser
    StrCpy $FpOldTorrent $FpSelTorrent
  ${EndIf}

  ${If} $UpdateMode <> 1
    ClearErrors
    ${GetOptions} $CMDLINE "/COMPONENTS=" $R0
    ${If} ${Errors}
      ; "/COMPONENTS" with no value is a mistake, not "no switch".
      ${StrCase} $R1 "$CMDLINE" "L"
      ${StrLoc} $R2 "$R1" "/components" ">"
      ${If} $R2 != ""
        StrCpy $FpReason "/COMPONENTS needs a value, such as /COMPONENTS=desktop,cli"
        Call FpRefuseSelection
      ${EndIf}
    ${Else}
      ${StrCase} $FpList "$R0" "L"
      ${If} $FpList == "full"
        StrCpy $FpType "full"
        Call FpSelAllVars
      ${Else}
        Call FpParseList
        ${If} $FpReason != ""
          Call FpRefuseSelection
        ${EndIf}
        StrCpy $FpType "custom"
      ${EndIf}
    ${EndIf}
  ${EndIf}
  Call FpApplySelection
FunctionEnd

; Writes the selection that was installed, canonical and sorted.
!macro FP_APPEND NAME
  ${If} $FpList == ""
    StrCpy $FpList "${NAME}"
  ${Else}
    StrCpy $FpList "$FpList,${NAME}"
  ${EndIf}
!macroend
Function FpWriteSelection
  StrCpy $FpList ""
  ${If} ${SectionIsSelected} ${SecBrowser}
    !insertmacro FP_APPEND "browser"
  ${EndIf}
  ${If} ${SectionIsSelected} ${SecCli}
    !insertmacro FP_APPEND "cli"
  ${EndIf}
  !insertmacro FP_APPEND "core"
  ${If} ${SectionIsSelected} ${SecDesktop}
    !insertmacro FP_APPEND "desktop"
  ${EndIf}
  ${If} ${SectionIsSelected} ${SecMcp}
    !insertmacro FP_APPEND "mcp"
  ${EndIf}
  ${If} ${SectionIsSelected} ${SecTorrent}
    !insertmacro FP_APPEND "torrent"
  ${EndIf}
  WriteRegStr SHCTX "${UNINSTKEY}" "FetchpathInstallType" "$FpType"
  WriteRegStr SHCTX "${UNINSTKEY}" "FetchpathComponents" "$FpList"
  WriteRegDWORD SHCTX "${UNINSTKEY}" "FetchpathComponentsSchema" 1
FunctionEnd

; --- difference removal ------------------------------------------------------

; Shortcuts whose target is the desktop app, found the way uninstall finds them.
Function FpRemoveDesktopShortcuts
  !insertmacro IsShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
  Pop $0
  ${If} $0 = 1
    !insertmacro UnpinShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
    Delete "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
    RMDir "$SMPROGRAMS\$AppStartMenuFolder"
  ${EndIf}
  !insertmacro IsShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
  Pop $0
  ${If} $0 = 1
    !insertmacro UnpinShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    Delete "$SMPROGRAMS\${PRODUCTNAME}.lnk"
  ${EndIf}
  !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
  Pop $0
  ${If} $0 = 1
    !insertmacro UnpinShortcut "$DESKTOP\${PRODUCTNAME}.lnk"
    Delete "$DESKTOP\${PRODUCTNAME}.lnk"
  ${EndIf}
FunctionEnd

; Removes what the previous selection had and the new one does not. Data,
; grants, history, media tools and the WebView2 profile are never touched. The
; PATH entry and the Terminal Start entry follow the new selection in the last
; section. /UPDATE keeps the stored selection and removes nothing.
Function FpRemoveDeselected
  ${If} $UpdateMode = 1
  ${OrIf} $FpHasOld <> 1
    Return
  ${EndIf}
  ${If} $FpOldDesktop = 1
  ${AndIfNot} ${SectionIsSelected} ${SecDesktop}
    DetailPrint "Removing the desktop app."
    Delete "$INSTDIR\${MAINBINARYNAME}.exe"
    Call FpRemoveDesktopShortcuts
  ${EndIf}
  ${If} $FpOldBrowser = 1
  ${AndIfNot} ${SectionIsSelected} ${SecBrowser}
    DetailPrint "Removing browser integration."
    !insertmacro FETCHPATH_REMOVE_HOST_KEYS
    Delete "$INSTDIR\fetchpath-browser-host.exe"
    {{#each resources}}
      !insertmacro FETCHPATH_RESOURCE_DELETE "browser" "{{this.[1]}}"
    {{/each}}
    RMDir "$INSTDIR\browser-extension\icons"
    RMDir "$INSTDIR\browser-extension"
  ${EndIf}
  ${If} $FpOldTorrent = 1
  ${AndIfNot} ${SectionIsSelected} ${SecTorrent}
    DetailPrint "Removing the torrent helper."
    Delete "$INSTDIR\fetchpath-torrent-helper.exe"
  ${EndIf}
FunctionEnd

; --- shortcuts ---------------------------------------------------------------

Function FpUpdateStartMenuEntries
  ${If} ${SectionIsSelected} ${SecDesktop}
    Call CreateOrUpdateStartMenuShortcut
  ${EndIf}
FunctionEnd

; "Fetchpath Terminal" exists while Terminal is selected and the desktop app is
; not; otherwise one left from an earlier selection goes.
Function FpUpdateTerminalEntry
  ${If} $UpdateMode = 1
    Return
  ${EndIf}
  !if "${STARTMENUFOLDER}" != ""
    !define FP_TERMINAL_LNK "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME} Terminal.lnk"
  !else
    !define FP_TERMINAL_LNK "$SMPROGRAMS\${PRODUCTNAME} Terminal.lnk"
  !endif
  ${If} ${SectionIsSelected} ${SecCli}
  ${AndIfNot} ${SectionIsSelected} ${SecDesktop}
    ${If} $NoShortcutMode <> 1
      !if "${STARTMENUFOLDER}" != ""
        CreateDirectory "$SMPROGRAMS\$AppStartMenuFolder"
      !endif
      CreateShortcut "${FP_TERMINAL_LNK}" "$INSTDIR\fetchpath.exe" "" "$INSTDIR\fetchpath.exe" 0
    ${EndIf}
  ${Else}
    !insertmacro IsShortcutTarget "${FP_TERMINAL_LNK}" "$INSTDIR\fetchpath.exe"
    Pop $0
    ${If} $0 = 1
      Delete "${FP_TERMINAL_LNK}"
    ${EndIf}
  ${EndIf}
  !undef FP_TERMINAL_LNK
FunctionEnd

Function FpComponentsLeave
  ${IfNot} ${SectionIsSelected} ${SecDesktop}
  ${AndIfNot} ${SectionIsSelected} ${SecCli}
    MessageBox MB_OK|MB_ICONEXCLAMATION "Choose the app or the terminal so you can manage Fetchpath."
    Abort
  ${EndIf}
FunctionEnd

; --- callbacks ---------------------------------------------------------------

Function .onInit
  ${GetOptions} $CMDLINE "/P" $PassiveMode
  ${IfNot} ${Errors}
    StrCpy $PassiveMode 1
  ${EndIf}

  ${GetOptions} $CMDLINE "/NS" $NoShortcutMode
  ${IfNot} ${Errors}
    StrCpy $NoShortcutMode 1
  ${EndIf}

  ${GetOptions} $CMDLINE "/UPDATE" $UpdateMode
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}

  !if "${DISPLAYLANGUAGESELECTOR}" == "true"
    !insertmacro MUI_LANGDLL_DISPLAY
  !endif

  !insertmacro SetContext

  ${If} $INSTDIR == "${PLACEHOLDER_INSTALL_DIR}"
    ; Set default install location
    !if "${INSTALLMODE}" == "perMachine"
      ${If} ${RunningX64}
        !if "${ARCH}" == "x64"
          StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"
        !else if "${ARCH}" == "arm64"
          StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"
        !else
          StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"
        !endif
      ${Else}
        StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"
      ${EndIf}
    !else if "${INSTALLMODE}" == "currentUser"
      StrCpy $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}"
    !endif

    Call RestorePreviousInstallLocation
  ${EndIf}


  !if "${INSTALLMODE}" == "both"
    !insertmacro MULTIUSER_INIT
  !endif

  ; FP-099: after the install folder is known, so a stored selection that
  ; cannot be read can be derived from the files there.
  Call FpInitSelection
FunctionEnd

; Terminal without a way to use it is never chosen by accident: asking for AI
; agents with neither the app nor the terminal adds the terminal.
Function .onSelChange
  ${If} ${SectionIsSelected} ${SecMcp}
  ${AndIfNot} ${SectionIsSelected} ${SecDesktop}
  ${AndIfNot} ${SectionIsSelected} ${SecCli}
    !insertmacro SelectSection ${SecCli}
  ${EndIf}
FunctionEnd

Function .onInstSuccess
  ; Check for `/R` flag only in silent and passive installers because
  ; GUI installer has a toggle for the user to (re)start the app
  ${If} $PassiveMode = 1
  ${OrIf} ${Silent}
    ${GetOptions} $CMDLINE "/R" $R0
    ${IfNot} ${Errors}
      ; FP-099: only the desktop app launches; a terminal-only install has no window.
      ${If} ${SectionIsSelected} ${SecDesktop}
        ${GetOptions} $CMDLINE "/ARGS" $R0
        nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" "$R0"
      ${EndIf}
    ${EndIf}
  ${EndIf}
FunctionEnd

; FP-099: the finish page says what was installed, and offers the desktop
; shortcut and "Run Fetchpath" only when the desktop app was.
Function FpFinishShow
  Call FpCheckDesktop
  StrCpy $0 "Fetchpath is installed."
  ${If} ${SectionIsSelected} ${SecCli}
    StrCpy $0 "$0$\r$\n$\r$\nThe fetchpath command is installed. Open a new terminal and type fetchpath --help to get started."
  ${EndIf}
  ${If} ${SectionIsSelected} ${SecMcp}
    StrCpy $0 "$0$\r$\n$\r$\nTo connect an AI agent, see the AI agents section of docs\CLI.md in the install folder. Agents get no access until you grant it."
  ${EndIf}
  StrCpy $0 "$0$\r$\n$\r$\nClick Finish to close Setup."
  SendMessage $mui.FinishPage.Text ${WM_SETTEXT} 0 "STR:$0"
  ${IfNot} ${SectionIsSelected} ${SecDesktop}
    SendMessage $mui.FinishPage.Run ${BM_SETCHECK} ${BST_UNCHECKED} 0
    SendMessage $mui.FinishPage.ShowReadme ${BM_SETCHECK} ${BST_UNCHECKED} 0
    ShowWindow $mui.FinishPage.Run ${SW_HIDE}
    ShowWindow $mui.FinishPage.ShowReadme ${SW_HIDE}
  ${EndIf}
FunctionEnd

Function un.onInit
  !insertmacro SetContext

  !if "${INSTALLMODE}" == "both"
    !insertmacro MULTIUSER_UNINIT
  !endif

  !insertmacro MUI_UNGETLANGUAGE

  ${GetOptions} $CMDLINE "/P" $PassiveMode
  ${IfNot} ${Errors}
    StrCpy $PassiveMode 1
  ${EndIf}

  ${GetOptions} $CMDLINE "/UPDATE" $UpdateMode
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}
FunctionEnd

Section Uninstall

  !ifmacrodef NSIS_HOOK_PREUNINSTALL
    !insertmacro NSIS_HOOK_PREUNINSTALL
  !endif

  ; FP-099: the hook above has stopped the engine and every other program
  ; running from this folder, so CheckIfAppIsRunning, which knew only the
  ; desktop exe, is not used.

  ; Delete the app directory and its content from disk
  ; The desktop app's own file (every component's files go below)
  Delete "$INSTDIR\${MAINBINARYNAME}.exe"

  ; Delete resources
  {{#each resources}}
    Delete "$INSTDIR\\{{this.[1]}}"
  {{/each}}

  ; Delete external binaries
  {{#each binaries}}
    Delete "$INSTDIR\\{{this}}"
  {{/each}}

  ; Delete app associations
  {{#each file_associations as |association| ~}}
    {{#each association.ext as |ext| ~}}
      !insertmacro APP_UNASSOCIATE "{{ext}}" "{{or association.name ext}}"
    {{/each}}
  {{/each}}

  ; Delete deep links
  {{#each deep_link_protocols as |protocol| ~}}
    ReadRegStr $R7 SHCTX "Software\Classes\\{{protocol}}\shell\open\command" ""
    ${If} $R7 == "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
      DeleteRegKey SHCTX "Software\Classes\\{{protocol}}"
    ${EndIf}
  {{/each}}


  ; Delete uninstaller
  Delete "$INSTDIR\uninstall.exe"

  {{#each resources_ancestors}}
  RMDir /REBOOTOK "$INSTDIR\\{{this}}"
  {{/each}}
  RMDir "$INSTDIR"

  ; Remove shortcuts if not updating
  ${If} $UpdateMode <> 1
    !insertmacro DeleteAppUserModelId

    ; Remove start menu shortcut
    !insertmacro MUI_STARTMENU_GETFOLDER Application $AppStartMenuFolder
    !insertmacro IsShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
      Delete "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
      RMDir "$SMPROGRAMS\$AppStartMenuFolder"
    ${EndIf}
    !insertmacro IsShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      Delete "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    ${EndIf}

    ; Remove desktop shortcuts
    !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$DESKTOP\${PRODUCTNAME}.lnk"
      Delete "$DESKTOP\${PRODUCTNAME}.lnk"
    ${EndIf}

    ; FP-099: the Terminal entry made when there is no desktop app targets fetchpath.exe.
    !insertmacro IsShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME} Terminal.lnk" "$INSTDIR\fetchpath.exe"
    Pop $0
    ${If} $0 = 1
      Delete "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME} Terminal.lnk"
      RMDir "$SMPROGRAMS\$AppStartMenuFolder"
    ${EndIf}
    !insertmacro IsShortcutTarget "$SMPROGRAMS\${PRODUCTNAME} Terminal.lnk" "$INSTDIR\fetchpath.exe"
    Pop $0
    ${If} $0 = 1
      Delete "$SMPROGRAMS\${PRODUCTNAME} Terminal.lnk"
    ${EndIf}
  ${EndIf}

  ; Remove registry information for add/remove programs
  !if "${INSTALLMODE}" == "both"
    DeleteRegKey SHCTX "${UNINSTKEY}"
  !else if "${INSTALLMODE}" == "perMachine"
    DeleteRegKey HKLM "${UNINSTKEY}"
  !else
    DeleteRegKey HKCU "${UNINSTKEY}"
  !endif

  ; Removes the Autostart entry for ${PRODUCTNAME} from the HKCU Run key if it exists.
  ; This ensures the program does not launch automatically after uninstallation if it exists.
  ; If it doesn't exist, it does nothing.
  ; We do this when not updating (to preserve the registry value on updates)
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
  ${EndIf}

  ; Delete app data if the checkbox is selected
  ; and if not updating
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ; Clear the install location $INSTDIR from registry
    DeleteRegKey SHCTX "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty SHCTX "${MANUKEY}"

    ; Clear the install language from registry
    DeleteRegValue HKCU "${MANUPRODUCTKEY}" "Installer Language"
    DeleteRegKey /ifempty HKCU "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty HKCU "${MANUKEY}"

    SetShellVarContext current
    RmDir /r "$APPDATA\${BUNDLEID}"
    RmDir /r "$LOCALAPPDATA\${BUNDLEID}"
  ${EndIf}

  !ifmacrodef NSIS_HOOK_POSTUNINSTALL
    !insertmacro NSIS_HOOK_POSTUNINSTALL
  !endif

  ; Auto close if passive mode or updating
  ${If} $PassiveMode = 1
  ${OrIf} $UpdateMode = 1
    SetAutoClose true
  ${EndIf}
SectionEnd

Function RestorePreviousInstallLocation
  ReadRegStr $4 SHCTX "${MANUPRODUCTKEY}" ""
  StrCmp $4 "" +2 0
    StrCpy $INSTDIR $4
FunctionEnd

Function Skip
  Abort
FunctionEnd

Function SkipIfPassive
  ${IfThen} $PassiveMode = 1  ${|} Abort ${|}
FunctionEnd
Function un.SkipIfPassive
  ${IfThen} $PassiveMode = 1  ${|} Abort ${|}
FunctionEnd

Function CreateOrUpdateStartMenuShortcut
  ; We used to use product name as MAINBINARYNAME
  ; migrate old shortcuts to target the new MAINBINARYNAME
  StrCpy $R0 0

  !insertmacro IsShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\$OldMainBinaryName"
  Pop $0
  ${If} $0 = 1
    !insertmacro SetShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    StrCpy $R0 1
  ${EndIf}

  !insertmacro IsShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\$OldMainBinaryName"
  Pop $0
  ${If} $0 = 1
    !insertmacro SetShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    StrCpy $R0 1
  ${EndIf}

  ${If} $R0 = 1
    Return
  ${EndIf}

  ; Skip creating shortcut if in update mode or no shortcut mode
  ; but always create if migrating from wix
  ${If} $WixMode = 0
    ${If} $UpdateMode = 1
    ${OrIf} $NoShortcutMode = 1
      Return
    ${EndIf}
  ${EndIf}

  !if "${STARTMENUFOLDER}" != ""
    CreateDirectory "$SMPROGRAMS\$AppStartMenuFolder"
    CreateShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
  !else
    CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
  !endif
FunctionEnd

Function CreateOrUpdateDesktopShortcut
  ${If} $FpDesktopOn <> 1
    Return
  ${EndIf}
  ; We used to use product name as MAINBINARYNAME
  ; migrate old shortcuts to target the new MAINBINARYNAME
  !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\$OldMainBinaryName"
  Pop $0
  ${If} $0 = 1
    !insertmacro SetShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Return
  ${EndIf}

  ; Skip creating shortcut if in update mode or no shortcut mode
  ; but always create if migrating from wix
  ${If} $WixMode = 0
    ${If} $UpdateMode = 1
    ${OrIf} $NoShortcutMode = 1
      Return
    ${EndIf}
  ${EndIf}

  CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
  !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
FunctionEnd
