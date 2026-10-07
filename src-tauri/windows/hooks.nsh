Var JuiceStatuslineQuarantined

; Return 1 for the exact installer payload, 0 for different bytes, 2 if unreadable.
Function JuiceStatuslineMatchesUpdatePayload
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  Push $6
  Push $7
  Push $8
  Push $9
  StrCpy $R9 2
  StrCpy $4 0
  StrCpy $5 0
  ClearErrors
  FileOpen $0 "$INSTDIR\agentjuice-statusline.exe" r
  IfErrors juice_statusline_compare_done
  FileOpen $1 "$PLUGINSDIR\juice-statusline-update-reference.exe" r
  IfErrors juice_statusline_compare_close_candidate
  FileSeek $0 0 END $2
  IfErrors juice_statusline_compare_close
  FileSeek $1 0 END $3
  IfErrors juice_statusline_compare_close
  IntCmp $3 0 juice_statusline_compare_close juice_statusline_compare_close
  IntCmp $3 134217728 0 0 juice_statusline_compare_close
  StrCmp $2 $3 0 juice_statusline_compare_different
  FileSeek $0 0 SET
  IfErrors juice_statusline_compare_close
  FileSeek $1 0 SET
  IfErrors juice_statusline_compare_close
  System::Alloc 65536
  Pop $4
  StrCmp $4 0 juice_statusline_compare_close
  System::Alloc 65536
  Pop $5
  StrCmp $5 0 juice_statusline_compare_close
  juice_statusline_compare_read:
    IntCmp $2 0 juice_statusline_compare_match
    StrCpy $3 65536
    IntCmp $2 65536 juice_statusline_compare_block juice_statusline_compare_last juice_statusline_compare_block
  juice_statusline_compare_last:
    StrCpy $3 $2
  juice_statusline_compare_block:
    System::Call 'kernel32::ReadFile(p r0, p r4, i r3, *i .r6, p 0) i .r8'
    StrCmp $8 0 juice_statusline_compare_close
    StrCmp $6 $3 0 juice_statusline_compare_close
    System::Call 'kernel32::ReadFile(p r1, p r5, i r3, *i .r7, p 0) i .r8'
    StrCmp $8 0 juice_statusline_compare_close
    StrCmp $7 $3 0 juice_statusline_compare_close
    System::Call 'msvcrt::memcmp(p r4, p r5, i r3) i .r8 ? c'
    StrCmp $8 0 0 juice_statusline_compare_different
    IntOp $2 $2 - $3
    Goto juice_statusline_compare_read
  juice_statusline_compare_match:
    StrCpy $R9 1
    Goto juice_statusline_compare_close
  juice_statusline_compare_different:
    StrCpy $R9 0
  juice_statusline_compare_close:
    StrCmp $4 0 +2
    System::Free $4
    StrCmp $5 0 +2
    System::Free $5
    FileClose $1
  juice_statusline_compare_close_candidate:
    FileClose $0
  juice_statusline_compare_done:
    Pop $9
    Pop $8
    Pop $7
    Pop $6
    Pop $5
    Pop $4
    Pop $3
    Pop $2
    Pop $1
    Pop $0
    Push $R9
FunctionEnd

Function .onInstFailed
  StrCmp $JuiceStatuslineQuarantined 1 0 juice_statusline_restore_done
  StrCpy $R8 0
  juice_statusline_restore_try:
    IfFileExists "$INSTDIR\agentjuice-statusline.juice-update-old.exe" 0 juice_statusline_restore_done
    IfFileExists "$INSTDIR\agentjuice-statusline.exe" 0 juice_statusline_restore_move
    Call JuiceStatuslineMatchesUpdatePayload
    Pop $R9
    StrCmp $R9 0 juice_statusline_restore_move juice_statusline_restore_done
  juice_statusline_restore_move:
    ; Atomic replacement leaves both files intact if restoring the old helper fails.
    System::Call 'kernel32::MoveFileExW(w "$INSTDIR\agentjuice-statusline.juice-update-old.exe", w "$INSTDIR\agentjuice-statusline.exe", i 9) i .s'
    Pop $R9
    StrCmp $R9 0 juice_statusline_restore_wait juice_statusline_restore_done
  juice_statusline_restore_wait:
    Sleep 50
    IntOp $R8 $R8 + 1
    IntCmp $R8 200 juice_statusline_restore_done juice_statusline_restore_try juice_statusline_restore_done
  juice_statusline_restore_done:
FunctionEnd

!macro NSIS_HOOK_PREINSTALL
  StrCpy $JuiceStatuslineQuarantined 0
  ; Tauri also checks after this hook; reject cancellation before moving the bridge.
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  ; Extract the trusted comparison source before changing the current installation.
  InitPluginsDir
  !searchreplace JuiceStatuslineUpdatePayloadPath "${MAINBINARYSRCPATH}" "${MAINBINARYNAME}.exe" "agentjuice-statusline.exe"
  ClearErrors
  File "/oname=$PLUGINSDIR\juice-statusline-update-reference.exe" "${JuiceStatuslineUpdatePayloadPath}"
  !undef JuiceStatuslineUpdatePayloadPath
  IfErrors 0 juice_statusline_update_reference_ready
  Abort "Juice could not prepare the status line recovery reference."
  juice_statusline_update_reference_ready:
  StrCpy $R8 0
  juice_statusline_quarantine_try:
    IfFileExists "$INSTDIR\agentjuice-statusline.exe" juice_statusline_quarantine_prepare juice_statusline_quarantine_done
  juice_statusline_quarantine_prepare:
    Delete "$INSTDIR\agentjuice-statusline.juice-update-old.exe"
    IfFileExists "$INSTDIR\agentjuice-statusline.juice-update-old.exe" juice_statusline_quarantine_wait juice_statusline_quarantine_move
  juice_statusline_quarantine_move:
    ClearErrors
    Rename "$INSTDIR\agentjuice-statusline.exe" "$INSTDIR\agentjuice-statusline.juice-update-old.exe"
    IfErrors juice_statusline_quarantine_wait juice_statusline_quarantine_moved
  juice_statusline_quarantine_moved:
    StrCpy $JuiceStatuslineQuarantined 1
    Goto juice_statusline_quarantine_done
  juice_statusline_quarantine_wait:
    Sleep 50
    IntOp $R8 $R8 + 1
    IntCmp $R8 200 juice_statusline_quarantine_timeout juice_statusline_quarantine_try juice_statusline_quarantine_timeout
  juice_statusline_quarantine_timeout:
    Abort "Juice could not prepare the Claude status line for update. Close active Claude status lines and try again."
  juice_statusline_quarantine_done:
!macroend

!macro NSIS_HOOK_POSTINSTALL
  Delete /REBOOTOK "$INSTDIR\agentjuice-statusline.juice-update-old.exe"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Tauri also checks after this hook; reject cancellation before restoring managed state.
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  ${If} $UpdateMode <> 1
    IfFileExists "$INSTDIR\agentjuice-statusline.exe" restore_owned_statusline restore_owned_statusline_missing_bridge
    restore_owned_statusline_missing_bridge:
      IfFileExists "$LOCALAPPDATA\agent-juice\wrap-meta.json" restore_owned_statusline_repair_required 0
      IfFileExists "$LOCALAPPDATA\agent-juice\antigravity-cli-binding.dpapi" restore_owned_statusline_repair_required restore_owned_statusline_done
    restore_owned_statusline_repair_required:
      MessageBox MB_OK|MB_ICONSTOP "Juice recovery metadata exists, but the status line bridge is missing. Repair or reinstall Juice before uninstalling." /SD IDOK
      Abort
    restore_owned_statusline:
      StrCpy $0 1
      ExecWait '"$INSTDIR\agentjuice-statusline.exe" --restore-owned-statusline' $0
    ${If} $0 <> 0
      MessageBox MB_OK|MB_ICONSTOP "Juice could not restore the managed status lines. Uninstall was stopped to preserve the bridge and recovery metadata." /SD IDOK
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
