!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    IfFileExists "$INSTDIR\agentjuice-statusline.exe" restore_owned_statusline restore_owned_statusline_missing_bridge
    restore_owned_statusline_missing_bridge:
      IfFileExists "$LOCALAPPDATA\agent-juice\wrap-meta.json" restore_owned_statusline_repair_required restore_owned_statusline_done
    restore_owned_statusline_repair_required:
      MessageBox MB_OK|MB_ICONSTOP "Juice recovery metadata exists, but the Claude status line bridge is missing. Repair or reinstall Juice before uninstalling." /SD IDOK
      Abort
    restore_owned_statusline:
      StrCpy $0 1
      ExecWait '"$INSTDIR\agentjuice-statusline.exe" --restore-owned-statusline' $0
    ${If} $0 <> 0
      MessageBox MB_OK|MB_ICONSTOP "Juice could not restore the Claude status line. Uninstall was stopped to preserve the bridge and recovery metadata." /SD IDOK
      Abort
    ${EndIf}
    restore_owned_statusline_done:
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    RmDir /r "$LOCALAPPDATA\agent-juice"
  ${EndIf}
!macroend
