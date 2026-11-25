; Where ARIA installs itself: %LOCALAPPDATA%\bin\ARIA.
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

; Silent: there is no window, hence no function above.
!macro NSIS_HOOK_PREINSTALL
  ${If} ${Silent}
    !insertmacro ARIA_DEFAULT_DIR
    SetOutPath $INSTDIR
  ${EndIf}
!macroend
