; Astrolabe — Windows installer.
;
; Hand-authored and versioned in the repository, deliberately. A generated
; installer is a build artifact nobody reads; this one is reviewed like code,
; because what it *registers* is exactly the surface a clean-machine test
; exercises.
;
; ---------------------------------------------------------------------------
; What changed, and why the file list went away
;
; Revision 7 of the Plan put the interface on Flutter over a Rust core. This
; script previously installed three files and said so in a comment that ended
; "no Flutter runtime, no flutter_windows.dll, no data/ payload, no bridge
; cdylib. None of those exist in this design any more." Every one of them
; exists now, and installing the old three produced a 92 KB runner stub with no
; engine, no Dart and no ICU — an install that cannot reach its first frame.
;
; The payload is still enumerated rather than copied wholesale, because what an
; installer places is the thing worth reading. The one exception is
; `data\flutter_assets\`, which nests a tree per package and has no hand-kept
; form that would survive a dependency shipping a font.
;
; The names below are what `flutter build windows --release` produces. If that
; set changes, this file is where it is noticed — which is the intent. The
; check that it stayed true lives in `tools/astrolabe/tests/packaging.rs`.
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
; Build:
;   flutter build windows --release            (in apps/astrolabe)
;   makensis -DVERSION=<x.y.z> -DSTAGE=<dir> -DSTUB=<file> \
;     packaging\windows\astrolabe.nsi
;
; where STAGE is the release bundle —
;   apps\astrolabe\build\windows\x64\runner\Release
; with THIRD-PARTY-NOTICES.md copied in beside it.

!include "MUI2.nsh"
!include "FileFunc.nsh"

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
  !error "STAGE must be passed: the Flutter release bundle directory"
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

  ; --- What the interface is made of ----------------------------------------
  ;
  ; `astrolabe.exe` is a 92 KB runner. Everything it actually is lives in these:
  ; the Rust core it calls across the bridge, the Flutter engine that draws, and
  ; the plugin DLLs behind the undecorated window and the tray icon.
  File "${STAGE}\astrolabe.dll"
  File "${STAGE}\flutter_windows.dll"
  File "${STAGE}\dartjni.dll"
  ; Flutter writes this manifest only when the bundle has native assets to
  ; describe. It existed in the developer build that originally proved this
  ; script, but Flutter 3.41 correctly omitted it from the tagged CI bundle.
  ; Carry it when present; its absence is not an incomplete application.
  File /nonfatal "${STAGE}\native_assets.json"
  File "${STAGE}\*_plugin.dll"

  ; The Visual C++ runtime, staged into the bundle by CMake's
  ; `InstallRequiredSystemLibraries`. astrolabe.exe imports MSVCP140.dll and
  ; VCRUNTIME140*.dll; a development machine has them because Visual Studio
  ; installed them, which is exactly why their absence is invisible right up
  ; until a clean machine, where the app dies in the loader with a dialog naming
  ; a DLL rather than this program. App-local rather than chaining the
  ; redistributable, because a per-user install that needs no elevation should
  ; not acquire a reason to ask for it.
  File "${STAGE}\msvcp140*.dll"
  File "${STAGE}\vcruntime140*.dll"
  File "${STAGE}\concrt140.dll"

  ; --- The payload the engine reads by path ---------------------------------
  ;
  ; `data\` must keep that name and that position: the engine resolves
  ; `data\icudtl.dat` and `data\flutter_assets\` relative to the executable, so
  ; an install that flattened or renamed them fails inside the loader rather
  ; than anywhere a person could act on. Recursive because `flutter_assets`
  ; nests per-package asset trees, which cannot be enumerated by hand and would
  ; go stale the first time a dependency shipped a font.
  SetOutPath "$INSTDIR\current\data"
  File /r "${STAGE}\data\*.*"
  SetOutPath "$INSTDIR"

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
  ;
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
