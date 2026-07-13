; LazySwitch's per-user Tauri installer hooks.
; The prompt intentionally only removes settings and cache. Enrolled accounts
; in ~/.codex-accounts and ~/.claude-accounts are never touched.

Var LazySwitchWipeData

!macro NSIS_HOOK_PREINSTALL
  StrCpy $LazySwitchWipeData "0"
  StrCpy $R1 "0"

  ; SHCTX is not pointed at the per-user hive yet when this hook runs, so read
  ; both roots explicitly — going through SHCTX silently finds nothing and the
  ; prompt never appears.
  ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\LazySwitch" "UninstallString"
  ${if} $R0 == ""
    ReadRegStr $R0 HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\LazySwitch" "UninstallString"
  ${endif}
  ${if} $R0 != ""
    StrCpy $R1 "1"
  ${endif}
  ${if} $R1 == "0"
    IfFileExists "$LOCALAPPDATA\LazySwitch\lazyswitch.exe" 0 +2
      StrCpy $R1 "1"
  ${endif}

  ${if} $R1 == "1"
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 \
      "LazySwitch가 이미 설치되어 있습니다.$\n$\n기존 설정과 캐시를 삭제하고 새로 설치할까요?$\n등록된 계정은 그대로 유지됩니다.$\n$\n[아니오]를 누르면 설정을 유지한 채 덮어씁니다." \
      /SD IDNO IDNO lazyswitch_keep_data
    StrCpy $LazySwitchWipeData "1"
    lazyswitch_keep_data:
  ${endif}

  ; The tray app is normally running during a reinstall, and Tauri's stock check
  ; would stop on a prompt whose Cancel aborts the install — so close it here.
  ; Going through cmd.exe with a /fi USERNAME filter costs ~55s on this machine;
  ; taskkill on its own is instant.
  nsExec::Exec `taskkill /f /im "lazyswitch.exe"`
  Pop $R9
  Delete "$INSTDIR\lazyswitch-cli.exe"
  Sleep 500
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Remove a payload directory left by an interrupted pre-1.0 bundle. This is
  ; installer-owned data, not user data.
  RMDir /r "$INSTDIR\cli-payload"
  ${if} $LazySwitchWipeData == "1"
    RMDir /r "$APPDATA\lazyswitch"
    RMDir /r "$PROFILE\.lazyswitch"
  ${endif}
  ; Tauri has no Electron-style runAfterFinish option. Launch from the final
  ; install hook so the app is ready when the finish page appears (and also
  ; when the installer is run silently).
  ExecShell "open" "$INSTDIR\lazyswitch.exe"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$INSTDIR\lazyswitch-cli.exe"
!macroend
