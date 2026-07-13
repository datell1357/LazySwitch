; Offer a clean reinstall when LazySwitch is already installed.
; Only settings and Chromium caches are removed — enrolled accounts live in
; ~/.codex-accounts and ~/.claude-accounts and are never touched here.
;
; electron-builder compiles this file twice; the uninstaller pass calls neither
; macro, so keep it out of that build or NSIS fails on the unused variable.
!ifndef BUILD_UNINSTALLER

Var LazySwitchWipeData

; customInit runs after initMultiUser, so SHCTX and $INSTDIR already point at
; the install this run would replace. UNINSTALL_APP_KEY is only the GUID —
; UNINSTALL_REGISTRY_KEY is the full path under the registry root.
!macro customInit
  StrCpy $LazySwitchWipeData "0"
  StrCpy $R1 "0"

  ReadRegStr $R0 SHCTX "${UNINSTALL_REGISTRY_KEY}" "UninstallString"
  ${if} $R0 != ""
    StrCpy $R1 "1"
  ${endif}
  ${if} $R1 == "0"
    IfFileExists "$INSTDIR\${APP_EXECUTABLE_FILENAME}" 0 +2
      StrCpy $R1 "1"
  ${endif}

  ${if} $R1 == "1"
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 \
      "LazySwitch가 이미 설치되어 있습니다.$\n$\n기존 설정과 캐시를 삭제하고 새로 설치할까요?$\n등록된 계정은 그대로 유지됩니다.$\n$\n[아니오]를 누르면 설정을 유지한 채 덮어씁니다." \
      /SD IDNO IDNO lazyswitch_keep_data
    StrCpy $LazySwitchWipeData "1"
    lazyswitch_keep_data:
  ${endif}
!macroend

; LazySwitch lives in the tray, so it is always running during a reinstall.
; The stock check stops on an "app is running" prompt whose Cancel aborts the
; whole install; close the app quietly instead.
!macro customCheckAppRunning
  nsExec::Exec `"$SYSDIR\cmd.exe" /c taskkill /f /t /im "${APP_EXECUTABLE_FILENAME}" /fi "USERNAME eq %USERNAME%"`
  Pop $R9
  Sleep 1500
!macroend

; Runs after the old app has been shut down and the files are in place, so the
; user-data directory is no longer locked.
!macro customInstall
  ${if} $LazySwitchWipeData == "1"
    RMDir /r "$APPDATA\lazyswitch"
    RMDir /r "$PROFILE\.lazyswitch"
  ${endif}
!macroend

!endif
