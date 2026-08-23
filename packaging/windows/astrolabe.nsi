; Astrolabe — Windows installer.
;
; Hand-authored and versioned in the repository, deliberately. A generated
; installer is a build artifact nobody reads; this one is reviewed like code,
; because what it *registers* is exactly the surface a clean-machine test
; exercises.
;
; ---------------------------------------------------------------------------
; The payload is the pair
;
; The client is the Tauri host: one executable drawing through the system
; WebView2, with `lait.exe` beside it. Both are built with a static C runtime
; (`build-astrolabe.sh` sets `+crt-static` for exactly this vehicle), so no
; msvcp/vcruntime DLLs travel and a clean machine cannot die in the loader.
; The one runtime dependency is WebView2 itself, ensured below.
;
; The payload is enumerated rather than copied wholesale, because what an
; installer places is the thing worth reading. The check that it stayed true
; lives in `tools/astrolabe/tests/packaging.rs`.
;
; ---------------------------------------------------------------------------
; The layout, and why the launcher does not move
;
; The installed program is a *stub* at the install root and a tree beneath it:
;
;   $INSTDIR\astrolabe.exe     the stub. The shortcut target, the protocol
;                              handler, the icon, and the path that never moves
;   $INSTDIR\current\          the release: the astrolabe+lait pair, flat
;   $INSTDIR\previous\         the prior release, kept bootable for rollback
;   $INSTDIR\staged\           a verified release waiting for the next launch
;
; The stub takes the *name* astrolabe.exe deliberately. Every shell artifact —
; the Start Menu shortcut, the `lait:` command, DisplayIcon, and any taskbar
; pin a person makes — keys on a path, and a path that changed per release is
; the single most expensive mistake in this space: Squirrel's app-1.0.0 /
; app-1.0.1 layout broke firewall rules, antivirus exclusions, GPU preferences
; and tray pinning, and it had to grow a routine that rewrote users' pinned
; shortcuts on every update. Nothing here may point into current\.
;
; The pair still sits flat and together inside current\, because that is where
; `sidecar::resolve` looks — relative to the running executable — and
; `update::custody_of` is its inverse. The stub is never between them.
;
; Build: `packaging/build-astrolabe.sh` assembles the stage and runs
;   makensis -DVERSION=<x.y.z> -DSTAGE=<dir> -DSTUB=<file> \
;     packaging\windows\astrolabe.nsi
; where STAGE holds the pair flat beside THIRD-PARTY-NOTICES.md and LICENSE —
; both are compiled in, and LICENSE is also read for the licence page, so a
; stage missing either fails the makensis run rather than shipping without
; them.

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !error "VERSION must be passed: makensis -DVERSION=x.y.z"
!endif
; VIProductVersion demands a numeric x.y.z, which a test prerelease
; (0.8.0-test.1) is not. The caller passes the numeric base separately; a plain
; release needs nothing and gets VERSION back.
!ifndef VERSION_NUMERIC
  !define VERSION_NUMERIC "${VERSION}"
!endif
!ifndef STAGE
  !error "STAGE must be passed: the directory holding the pair and the terms"
!endif
!ifndef STUB
  !error "STUB must be passed: the built astrolabe-stub executable"
!endif
; NSIS resolves a relative OutFile against the SCRIPT's directory, not the
; working directory — so `cd dist && makensis ..\packaging\windows\astrolabe.nsi`
; wrote the installer into packaging\windows\ and the caller, looking in dist\,
; reported "makensis produced no installer" for a compile that had in fact
; succeeded. It read like a build failure and was a path convention. The caller
; passes OUTDIR absolute; standalone invocations keep the old behaviour.
!ifndef OUTDIR
  !define OUTDIR "."
!endif

Name "Astrolabe"
OutFile "${OUTDIR}\astrolabe-${VERSION}-setup.exe"
Unicode true

; Per-user by default: Astrolabe is a single-user client that manages a
; per-identity daemon and a state root under the user's profile. Installing it
; machine-wide would require elevation for something that has no machine-wide
; effect, and elevation is the one prompt people learn to click through.
InstallDir "$LOCALAPPDATA\Programs\Astrolabe"
RequestExecutionLevel user
SetCompressor /SOLID lzma

VIProductVersion "${VERSION_NUMERIC}.0"
VIAddVersionKey "ProductName" "Astrolabe"
VIAddVersionKey "FileDescription" "The local client for served Worlds"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Copyright 2026 Omar Younes - PolyForm Noncommercial 1.0.0"

!define MUI_ABORTWARNING
; The terms, shown before anything is installed. PolyForm obliges whoever
; distributes a copy to ensure the recipient also gets them; a file dropped in
; the install directory satisfies that only in the letter, and this product is
; noncommercial-licensed, which is the kind of term a person installing it at
; work needs to see rather than discover. Read from ${STAGE} for the same
; reason the notices are: CI stages both beside the pair, so the installer and
; the update tree carry the same file.
!insertmacro MUI_PAGE_LICENSE "${STAGE}\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

!define REGKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Astrolabe"

Section "Astrolabe" SecMain
  SectionIn RO

  ; --- The stub -------------------------------------------------------------
  ;
  ; The one file an update never moves, and the only thing anything outside
  ; this install ever points at. It verifies whatever is staged, swaps it in by
  ; rename when no client holds the installation, and starts what is current.
  SetOutPath "$INSTDIR"
  File "/oname=astrolabe.exe" "${STUB}"

  ; --- The release ----------------------------------------------------------
  SetOutPath "$INSTDIR\current"

  ; --- The two programs -----------------------------------------------------
  ;
  ; The pair ships together and is installed together. `lait.exe` lands *beside*
  ; astrolabe.exe because that is where `sidecar::resolve` looks — the rule is
  ; "relative to the running executable", and this is the other half of it.
  File "${STAGE}\astrolabe.exe"
  File "${STAGE}\lait.exe"

  ; What a person who receives these binaries is owed: every crate they are
  ; built from, and the terms it is offered under. Generated from the lockfile
  ; by `ci/third-party-notices.sh` and held current by CI, so shipping it is a
  ; copy rather than something somebody has to remember to update.
  File "${STAGE}\THIRD-PARTY-NOTICES.md"
  File "${STAGE}\LICENSE"

  SetOutPath "$INSTDIR"

  ; --- WebView2 -------------------------------------------------------------
  ;
  ; The one runtime the pair does not carry: the host draws through the
  ; system's Evergreen WebView2, updated by the OS on the OS's schedule —
  ; which is the point. Windows 11 ships it; a machine without it gets
  ; Microsoft's own bootstrapper, staged by `build-astrolabe.sh`, run
  ; un-silenced so its elevation prompt is its own and this install stays
  ; unelevated.
  ClearErrors
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${If} ${Errors}
    ClearErrors
    ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${EndIf}
  ${If} ${Errors}
    InitPluginsDir
    File /nonfatal "/oname=$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe" "${STAGE}\MicrosoftEdgeWebview2Setup.exe"
    ${If} ${FileExists} "$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe"
      ExecWait '"$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe"'
    ${Else}
      MessageBox MB_OK "Astrolabe draws through Microsoft WebView2, which this machine does not have. Install it from https://developer.microsoft.com/microsoft-edge/webview2/ and launch Astrolabe again."
    ${EndIf}
  ${EndIf}

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

  ; Agent/editor bindings deliberately contain the portable command
  ; `lait mcp`, not this machine's absolute install path. Register the stable
  ; current/ coordinate after every file and registry write has landed, so a
  ; clean machine resolves the sidecar this installer owns and never needs a
  ; Cargo-installed copy. The stub performs the registry edit natively: NSIS's
  ; ordinary 1024-character strings can truncate an existing user PATH.
  ExecWait '"$INSTDIR\astrolabe.exe" --install-command-path' $0
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "Astrolabe was installed, but its lait command could not be registered for this user. The installation has been left intact; run this installer again to repair it."
    Abort
  ${EndIf}
SectionEnd

Section "Uninstall"
  ; Uninstalling removes the program. It does *not* remove the person's Spaces,
  ; their identity, or anything under the managed state root — removal and data
  ; deletion are separate operations here for the same reason they are separate
  ; for devices, and an uninstaller that quietly destroyed a store would be the
  ; worst possible place to conflate them.
  ;
  ; Remove only our exact command directory while the stable stub still
  ; exists to perform the full-length registry edit. Refuse to remove the
  ; program if this fails: a stale PATH entry is an installed surface too.
  ExecWait '"$INSTDIR\astrolabe.exe" --uninstall-command-path' $0
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "Astrolabe could not remove its lait command from this user's PATH, so no program files were removed."
    Abort
  ${EndIf}

  Delete "$INSTDIR\astrolabe.exe"
  Delete "$INSTDIR\uninstall.exe"
  Delete "$INSTDIR\instance.lock"
  Delete "$INSTDIR\staging.lock"
  Delete "$INSTDIR\stub.log"
  Delete "$INSTDIR\staged.manifest.json"

  ; Three recursive removals, each bounded to a release tree by name.
  ;
  ; A release is a tree the installer did not enumerate and could not: it is
  ; whatever the update path put there, and after the first update it is not
  ; even what this installer shipped. So the scope is stated per directory
  ; rather than by listing files — and never as `RMDir /r "$INSTDIR"`, which
  ; would carry away anything else that happened to be in the install root.
  ; Nothing a person owns has ever been written under these: state lives under
  ; the user's profile, and removal is not deletion.
  RMDir /r "$INSTDIR\current"
  RMDir /r "$INSTDIR\previous"
  RMDir /r "$INSTDIR\staged"

  ; $INSTDIR itself comes off only with plain `RMDir`, which refuses a
  ; directory that still holds something: if a future build leaves a file this
  ; section does not know about, the install directory survives and says so.
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\Astrolabe.lnk"

  DeleteRegKey HKCU "Software\Classes\lait"
  DeleteRegKey HKCU "${REGKEY}"
SectionEnd
