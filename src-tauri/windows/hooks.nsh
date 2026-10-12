; Installer hooks.
;
; Quota registers itself in Holster's dock with a manifest in
; %APPDATA%\Holster\apps. Uninstalling takes the tile out of the dock with it
; (Holster watches that folder and drops the tile live). An update runs the
; old uninstaller too, so the manifest stays put then: the new Quota rewrites
; it when it starts anyway.

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    Delete "$APPDATA\Holster\apps\quota.json"
  ${EndIf}
!macroend
