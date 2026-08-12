; Astrolabe — Windows installer.
;
; Hand-authored and versioned in the repository, deliberately. A generated
; installer is a build artifact nobody reads; this one is reviewed like code,
; because what it installs and what it registers is exactly the surface a
; clean-machine test exercises.
;
; What it installs, and nothing else:
;   astrolabe.exe   the client
;   lait.exe        the fixed sidecar, beside it — see src/sidecar.rs
;
; What it deliberately does NOT install: no Flutter runtime, no
; flutter_windows.dll, no data/ payload, no bridge cdylib, no WebView2
; bootstrap. None of those exist in this design any more, and an installer that
; still carried them would be the clearest possible sign the old stack had
; survived somewhere.
;
; Build:
;   makensis -DVERSION=<x.y.z> -DSTAGE=<dir> packaging\windows\astrolabe.nsi
; where STAGE holds astrolabe.exe and lait.exe.

!include "MUI2.nsh"
!include "FileFunc.nsh"

!ifndef VERSION
  !error "VERSION must be passed: makensis -DVERSION=x.y.z"
!endif
!ifndef STAGE
  !error "STAGE must be passed: the directory holding astrolabe.exe and lait.exe"
!endif

Name "Astrolabe"
OutFile "astrolabe-${VERSION}-setup.exe"
Unicode true

; Per-user by default: Astrolabe is a single-user client that manages a
; per-identity daemon and a state root under the user's profile. Installing it
; machine-wide would require elevation for something that has no machine-wide
; effect, and elevation is the one prompt people learn to click through.
InstallDir "$LOCALAPPDATA\Programs\Astrolabe"
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Astrolabe"
VIAddVersionKey "FileDescription" "The local client for served Worlds"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "MIT OR Apache-2.0"

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

!define REGKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Astrolabe"

Section "Astrolabe" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"

  ; The pair ships together and is installed together. `lait.exe` lands *beside*
  ; astrolabe.exe because that is where `sidecar::resolve` looks — the rule is
  ; "relative to the running executable", and this is the other half of it.
  File "${STAGE}\astrolabe.exe"
  File "${STAGE}\lait.exe"

  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateShortcut "$SMPROGRAMS\Astrolabe.lnk" "$INSTDIR\astrolabe.exe"

  ; The `lait:` scheme, so an invite can be a link rather than a blob somebody
  ; copies between two places and truncates on the way. Registered per-user
  ; under Software\Classes, which is what a per-user install may write.
  ;
  ; A link is an *input*, not an authority: it carries a ticket to a flow that
  ; already validates it, and opening the client is not accepting an invite.
  WriteRegStr HKCU "Software\Classes\lait" "" "URL:lait Protocol"
  WriteRegStr HKCU "Software\Classes\lait" "URL Protocol" ""
  WriteRegStr HKCU "Software\Classes\lait\DefaultIcon" "" "$INSTDIR\astrolabe.exe,0"
  WriteRegStr HKCU "Software\Classes\lait\shell\open\command" "" '"$INSTDIR\astrolabe.exe" "%1"'

  WriteRegStr HKCU "${REGKEY}" "DisplayName" "Astrolabe"
  WriteRegStr HKCU "${REGKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${REGKEY}" "Publisher" "Nixie Tech"
  WriteRegStr HKCU "${REGKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${REGKEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKCU "${REGKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${REGKEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "${REGKEY}" "EstimatedSize" "$0"
SectionEnd

Section "Uninstall"
  ; Uninstalling removes the program. It does *not* remove the person's Spaces,
  ; their identity, or anything under the managed state root — removal and data
  ; deletion are separate operations here for the same reason they are separate
  ; for devices, and an uninstaller that quietly destroyed a store would be the
  ; worst possible place to conflate them.
  Delete "$INSTDIR\astrolabe.exe"
  Delete "$INSTDIR\lait.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\Astrolabe.lnk"

  DeleteRegKey HKCU "Software\Classes\lait"
  DeleteRegKey HKCU "${REGKEY}"
SectionEnd
