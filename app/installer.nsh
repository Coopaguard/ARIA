; What Tauri's installer does not do by itself.
;
; 1. Where ARIA installs itself: %LOCALAPPDATA%\bin\ARIA.
;
; Tauri's default is %LOCALAPPDATA%\ARIA, which is also the folder of the models
; (Windows ignores case): emptying it to free 5 GB would remove the program.
; Tauri has no setting for it, and its template is too long to copy, so the
; default is replaced here, once Tauri's `.onInit` has chosen.
;
; A copy already installed in the old folder stays there, and is updated there.

!include LogicLib.nsh

!macro ARIA_DEFAULT_DIR
  ${If} $INSTDIR == "$LOCALAPPDATA\ARIA"
  ${AndIfNot} ${FileExists} "$LOCALAPPDATA\ARIA\aria.exe"
    StrCpy $INSTDIR "$LOCALAPPDATA\bin\ARIA"
  ${EndIf}
!macroend

; With a window: before the page that shows the folder. The user's own choice,
; made later, is kept.
!define MUI_CUSTOMFUNCTION_GUIINIT AriaDefaultDir
Function AriaDefaultDir
  !insertmacro ARIA_DEFAULT_DIR
FunctionEnd

; 2. An update waits for ARIA to be gone.
;
; The updater starts this installer and ends ARIA at once. Tauri then tries to
; close it, once, and gives up with "Failed to kill ARIA": a process whose memory
; Windows is still releasing cannot be closed, and ARIA holds 3 GB
; (tauri-apps/tauri#16277). A running program cannot be opened for writing: once
; it can, ARIA is gone. After 20 s, Tauri's own check decides.
!macro ARIA_WAIT_FOR_EXIT
  Push $R8
  Push $R9
  StrCpy $R8 0
  ${DoWhile} $R8 < 100
    ${IfNot} ${FileExists} "$INSTDIR\aria.exe"
      ${ExitDo}
    ${EndIf}
    ClearErrors
    FileOpen $R9 "$INSTDIR\aria.exe" a
    ${IfNot} ${Errors}
      FileClose $R9
      ${ExitDo}
    ${EndIf}
    Sleep 200
    IntOp $R8 $R8 + 1
  ${Loop}
  ClearErrors
  Pop $R9
  Pop $R8
!macroend

; Silent: there is no window, hence no function above.
!macro NSIS_HOOK_PREINSTALL
  ${If} ${Silent}
    !insertmacro ARIA_DEFAULT_DIR
    SetOutPath $INSTDIR
  ${EndIf}
  ${If} $UpdateMode = 1
    !insertmacro ARIA_WAIT_FOR_EXIT
  ${EndIf}
!macroend
