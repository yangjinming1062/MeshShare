!macro NSIS_HOOK_PREUNINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "MeshShare"
  DeleteRegValue HKLM "Software\Microsoft\Windows\CurrentVersion\Run" "MeshShare"
!macroend
