; Hooks for the Windows installer, see `bundle.windows.nsis.installerHooks` in tauri.conf.json

; Remove the "Start at login" entry, so it doesn't point at the removed exe. Kept when the uninstaller runs as part
; of an update.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "AutoKeyLight"
  ${EndIf}
!macroend
