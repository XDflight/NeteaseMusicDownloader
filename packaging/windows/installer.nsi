; NSIS installer for 云音下载姬 (Netease Music Downloader).
;
;   makensis /INPUTCHARSET UTF8 /DVERSION=0.1.0 /DVERSION_QUAD=0.1.0.0 ^
;            /DEXE_PATH=..\..\target\release\netease-music-downloader.exe ^
;            /DOUT_FILE=netease-music-downloader-0.1.0-windows-x86_64-setup.exe installer.nsi
;
; Per-user installation (no administrator rights), so the in-app updater can run this installer
; silently:  setup.exe /S /UPDATE   waits for the running app to exit, upgrades in place and
; starts the new version. The uninstaller is called unins000.exe: the updater uses its presence
; to recognise an installed (as opposed to portable) copy.

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef VERSION_QUAD
  !define VERSION_QUAD "0.0.0.0"
!endif
!ifndef EXE_PATH
  !define EXE_PATH "..\..\target\release\netease-music-downloader.exe"
!endif
!ifndef OUT_FILE
  !define OUT_FILE "netease-music-downloader-setup.exe"
!endif

!define APP_NAME "云音下载姬"
!define APP_ID "NeteaseMusicDownloader"
!define APP_EXE "netease-music-downloader.exe"
!define UNINSTALLER "unins000.exe"
!define WINDOW_TITLE "云音下载姬 · 网易云音乐批量下载器"
!define PUBLISHER "XDflight"
!define HOMEPAGE "https://github.com/XDflight/NeteaseMusicDownloader"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_ID}"

Name "${APP_NAME}"
OutFile "${OUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\${APP_ID}"
InstallDirRegKey HKCU "Software\${APP_ID}" "InstallDir"
BrandingText "${APP_NAME} ${VERSION}"

VIProductVersion "${VERSION_QUAD}"
VIAddVersionKey /LANG=0 "ProductName" "${APP_NAME}"
VIAddVersionKey /LANG=0 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=0 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=0 "CompanyName" "${PUBLISHER}"
VIAddVersionKey /LANG=0 "FileDescription" "${APP_NAME} setup"
VIAddVersionKey /LANG=0 "LegalCopyright" "AGPL-3.0-only"

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"

!define MUI_ICON "..\icons\app.ico"
!define MUI_UNICON "..\icons\app.ico"
!define MUI_WELCOMEFINISHPAGE_BITMAP "wizard_large.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "wizard_large.bmp"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_BITMAP "wizard_header.bmp"
!define MUI_HEADERIMAGE_RIGHT
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "$(RunApp)"

Var UpdateMode

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

LangString SecMain ${LANG_SIMPCHINESE} "主程序（必选）"
LangString SecMain ${LANG_ENGLISH} "Application (required)"
LangString SecDesktop ${LANG_SIMPCHINESE} "桌面快捷方式"
LangString SecDesktop ${LANG_ENGLISH} "Desktop shortcut"
LangString RunApp ${LANG_SIMPCHINESE} "启动 ${APP_NAME}"
LangString RunApp ${LANG_ENGLISH} "Start ${APP_NAME}"
LangString CloseFirst ${LANG_SIMPCHINESE} "${APP_NAME} 正在运行。请先关闭它，然后点击「确定」继续。"
LangString CloseFirst ${LANG_ENGLISH} "${APP_NAME} is running. Please close it, then click OK to continue."
LangString KeepDataAsk ${LANG_SIMPCHINESE} "是否同时删除设置和已保存的登录信息？$\n（选择「否」可保留，重新安装后仍可使用。）"
LangString KeepDataAsk ${LANG_ENGLISH} "Also delete your settings and saved login?$\n(Choose No to keep them for a future reinstall.)"
LangString UninstallLink ${LANG_SIMPCHINESE} "卸载 ${APP_NAME}"
LangString UninstallLink ${LANG_ENGLISH} "Uninstall ${APP_NAME}"

Function .onInit
  StrCpy $UpdateMode 0
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/UPDATE" $R1
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}

  ; A normal (interactive) install must not overwrite a running program.
  ${If} $UpdateMode == 0
    ${Do}
      FindWindow $R2 "" "${WINDOW_TITLE}"
      ${If} $R2 == 0
        ${Break}
      ${EndIf}
      MessageBox MB_OKCANCEL|MB_ICONEXCLAMATION "$(CloseFirst)" /SD IDCANCEL IDOK +2
      Abort
    ${Loop}
  ${EndIf}
FunctionEnd

Section "$(SecMain)" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"

  ; During an update the old process may still be shutting down: retry for up to 20 seconds.
  StrCpy $R0 0
  ${Do}
    ClearErrors
    Delete "$INSTDIR\${APP_EXE}"
    ${IfNot} ${Errors}
      ${Break}
    ${EndIf}
    IntOp $R0 $R0 + 1
    ${If} $R0 >= 40
      ${Break}
    ${EndIf}
    Sleep 500
  ${Loop}

  ; The installed name is fixed; it must not depend on what the build artifact is called.
  File "/oname=${APP_EXE}" "${EXE_PATH}"
  File "..\..\LICENSE"
  WriteUninstaller "$INSTDIR\${UNINSTALLER}"

  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\$(UninstallLink).lnk" "$INSTDIR\${UNINSTALLER}"

  WriteRegStr HKCU "Software\${APP_ID}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "${HOMEPAGE}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\${UNINSTALLER}"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\${UNINSTALLER}" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" "$0"
SectionEnd

Section "$(SecDesktop)" SecDesktop
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
SectionEnd

Function .onInstSuccess
  ${If} $UpdateMode == 1
    Exec '"$INSTDIR\${APP_EXE}"'
  ${EndIf}
FunctionEnd

Section "Uninstall"
  ${Do}
    ClearErrors
    Delete "$INSTDIR\${APP_EXE}"
    ${IfNot} ${Errors}
      ${Break}
    ${EndIf}
    MessageBox MB_OKCANCEL|MB_ICONEXCLAMATION "$(CloseFirst)" /SD IDCANCEL IDOK +2
    Abort
  ${Loop}

  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\${UNINSTALLER}"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  Delete "$SMPROGRAMS\${APP_NAME}\$(UninstallLink).lnk"
  RMDir "$SMPROGRAMS\${APP_NAME}"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  DeleteRegKey HKCU "Software\${APP_ID}"

  MessageBox MB_YESNO|MB_ICONQUESTION "$(KeepDataAsk)" /SD IDNO IDNO keep_data
    RMDir /r "$APPDATA\${PUBLISHER}\${APP_ID}"
    RMDir /r "$LOCALAPPDATA\${PUBLISHER}\${APP_ID}"
    RMDir "$APPDATA\${PUBLISHER}"
    RMDir "$LOCALAPPDATA\${PUBLISHER}"
  keep_data:
SectionEnd
