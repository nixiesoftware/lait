// GENERATED CODE - DO NOT MODIFY BY HAND
// coverage:ignore-file
// ignore_for_file: type=lint
// ignore_for_file: unused_element, deprecated_member_use, deprecated_member_use_from_same_package, use_function_type_syntax_for_parameters, unnecessary_const, avoid_init_to_null, invalid_override_different_default_values_named, prefer_expression_function_bodies, annotate_overrides, invalid_annotation_target, unnecessary_question_mark

part of 'api.dart';

// **************************************************************************
// FreezedGenerator
// **************************************************************************

// dart format off
T _$identity<T>(T value) => value;

/// @nodoc
mixin _$ActionRequest {
  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType && other is ActionRequest);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'ActionRequest()';
  }
}

/// @nodoc
class $ActionRequestCopyWith<$Res> {
  $ActionRequestCopyWith(ActionRequest _, $Res Function(ActionRequest) __);
}

/// Adds pattern-matching-related methods to [ActionRequest].
extension ActionRequestPatterns on ActionRequest {
  /// A variant of `map` that fallback to returning `orElse`.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case _:
  ///     return orElse();
  /// }
  /// ```

  @optionalTypeArgs
  TResult maybeMap<TResult extends Object?>({
    TResult Function(ActionRequest_Refresh value)? refresh,
    TResult Function(ActionRequest_Open value)? open,
    TResult Function(ActionRequest_StartDevice value)? startDevice,
    TResult Function(ActionRequest_StopDevice value)? stopDevice,
    TResult Function(ActionRequest_RestartDevice value)? restartDevice,
    TResult Function(ActionRequest_ForceStopDevice value)? forceStopDevice,
    TResult Function(ActionRequest_StopAllOwned value)? stopAllOwned,
    TResult Function(ActionRequest_RemoveDevice value)? removeDevice,
    TResult Function(ActionRequest_ReadSpace value)? readSpace,
    TResult Function(ActionRequest_StartHead value)? startHead,
    TResult Function(ActionRequest_StopHead value)? stopHead,
    TResult Function(ActionRequest_ForgetOrbit value)? forgetOrbit,
    TResult Function(ActionRequest_BookPut value)? bookPut,
    TResult Function(ActionRequest_BookDelete value)? bookDelete,
    TResult Function(ActionRequest_BookSetPicture value)? bookSetPicture,
    TResult Function(ActionRequest_BookMerge value)? bookMerge,
    TResult Function(ActionRequest_BookClaimSelf value)? bookClaimSelf,
    TResult Function(ActionRequest_BookLink value)? bookLink,
    TResult Function(ActionRequest_BookUnlink value)? bookUnlink,
    TResult Function(ActionRequest_BookExport value)? bookExport,
    TResult Function(ActionRequest_BookImport value)? bookImport,
    TResult Function(ActionRequest_BookAccept value)? bookAccept,
    TResult Function(ActionRequest_BookDismiss value)? bookDismiss,
    TResult Function(ActionRequest_InstallMcp value)? installMcp,
    TResult Function(ActionRequest_DisplayPairingApprove value)?
        displayPairingApprove,
    TResult Function(ActionRequest_DisplayPairingReject value)?
        displayPairingReject,
    TResult Function(ActionRequest_DisplayAssignmentPut value)?
        displayAssignmentPut,
    TResult Function(ActionRequest_DisplayAssignmentRevoke value)?
        displayAssignmentRevoke,
    TResult Function(ActionRequest_DisplayDeviceRevoke value)?
        displayDeviceRevoke,
    TResult Function(ActionRequest_DisplayIdentifierAdmitPassphrase value)?
        displayIdentifierAdmitPassphrase,
    TResult Function(ActionRequest_EnterPresentation value)? enterPresentation,
    TResult Function(ActionRequest_PresentHere value)? presentHere,
    TResult Function(ActionRequest_PresentRefresh value)? presentRefresh,
    TResult Function(ActionRequest_LeavePresentation value)? leavePresentation,
    required TResult orElse(),
  }) {
    final _that = this;
    switch (_that) {
      case ActionRequest_Refresh() when refresh != null:
        return refresh(_that);
      case ActionRequest_Open() when open != null:
        return open(_that);
      case ActionRequest_StartDevice() when startDevice != null:
        return startDevice(_that);
      case ActionRequest_StopDevice() when stopDevice != null:
        return stopDevice(_that);
      case ActionRequest_RestartDevice() when restartDevice != null:
        return restartDevice(_that);
      case ActionRequest_ForceStopDevice() when forceStopDevice != null:
        return forceStopDevice(_that);
      case ActionRequest_StopAllOwned() when stopAllOwned != null:
        return stopAllOwned(_that);
      case ActionRequest_RemoveDevice() when removeDevice != null:
        return removeDevice(_that);
      case ActionRequest_ReadSpace() when readSpace != null:
        return readSpace(_that);
      case ActionRequest_StartHead() when startHead != null:
        return startHead(_that);
      case ActionRequest_StopHead() when stopHead != null:
        return stopHead(_that);
      case ActionRequest_ForgetOrbit() when forgetOrbit != null:
        return forgetOrbit(_that);
      case ActionRequest_BookPut() when bookPut != null:
        return bookPut(_that);
      case ActionRequest_BookDelete() when bookDelete != null:
        return bookDelete(_that);
      case ActionRequest_BookSetPicture() when bookSetPicture != null:
        return bookSetPicture(_that);
      case ActionRequest_BookMerge() when bookMerge != null:
        return bookMerge(_that);
      case ActionRequest_BookClaimSelf() when bookClaimSelf != null:
        return bookClaimSelf(_that);
      case ActionRequest_BookLink() when bookLink != null:
        return bookLink(_that);
      case ActionRequest_BookUnlink() when bookUnlink != null:
        return bookUnlink(_that);
      case ActionRequest_BookExport() when bookExport != null:
        return bookExport(_that);
      case ActionRequest_BookImport() when bookImport != null:
        return bookImport(_that);
      case ActionRequest_BookAccept() when bookAccept != null:
        return bookAccept(_that);
      case ActionRequest_BookDismiss() when bookDismiss != null:
        return bookDismiss(_that);
      case ActionRequest_InstallMcp() when installMcp != null:
        return installMcp(_that);
      case ActionRequest_DisplayPairingApprove()
          when displayPairingApprove != null:
        return displayPairingApprove(_that);
      case ActionRequest_DisplayPairingReject()
          when displayPairingReject != null:
        return displayPairingReject(_that);
      case ActionRequest_DisplayAssignmentPut()
          when displayAssignmentPut != null:
        return displayAssignmentPut(_that);
      case ActionRequest_DisplayAssignmentRevoke()
          when displayAssignmentRevoke != null:
        return displayAssignmentRevoke(_that);
      case ActionRequest_DisplayDeviceRevoke() when displayDeviceRevoke != null:
        return displayDeviceRevoke(_that);
      case ActionRequest_DisplayIdentifierAdmitPassphrase()
          when displayIdentifierAdmitPassphrase != null:
        return displayIdentifierAdmitPassphrase(_that);
      case ActionRequest_EnterPresentation() when enterPresentation != null:
        return enterPresentation(_that);
      case ActionRequest_PresentHere() when presentHere != null:
        return presentHere(_that);
      case ActionRequest_PresentRefresh() when presentRefresh != null:
        return presentRefresh(_that);
      case ActionRequest_LeavePresentation() when leavePresentation != null:
        return leavePresentation(_that);
      case _:
        return orElse();
    }
  }

  /// A `switch`-like method, using callbacks.
  ///
  /// Callbacks receives the raw object, upcasted.
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case final Subclass2 value:
  ///     return ...;
  /// }
  /// ```

  @optionalTypeArgs
  TResult map<TResult extends Object?>({
    required TResult Function(ActionRequest_Refresh value) refresh,
    required TResult Function(ActionRequest_Open value) open,
    required TResult Function(ActionRequest_StartDevice value) startDevice,
    required TResult Function(ActionRequest_StopDevice value) stopDevice,
    required TResult Function(ActionRequest_RestartDevice value) restartDevice,
    required TResult Function(ActionRequest_ForceStopDevice value)
        forceStopDevice,
    required TResult Function(ActionRequest_StopAllOwned value) stopAllOwned,
    required TResult Function(ActionRequest_RemoveDevice value) removeDevice,
    required TResult Function(ActionRequest_ReadSpace value) readSpace,
    required TResult Function(ActionRequest_StartHead value) startHead,
    required TResult Function(ActionRequest_StopHead value) stopHead,
    required TResult Function(ActionRequest_ForgetOrbit value) forgetOrbit,
    required TResult Function(ActionRequest_BookPut value) bookPut,
    required TResult Function(ActionRequest_BookDelete value) bookDelete,
    required TResult Function(ActionRequest_BookSetPicture value)
        bookSetPicture,
    required TResult Function(ActionRequest_BookMerge value) bookMerge,
    required TResult Function(ActionRequest_BookClaimSelf value) bookClaimSelf,
    required TResult Function(ActionRequest_BookLink value) bookLink,
    required TResult Function(ActionRequest_BookUnlink value) bookUnlink,
    required TResult Function(ActionRequest_BookExport value) bookExport,
    required TResult Function(ActionRequest_BookImport value) bookImport,
    required TResult Function(ActionRequest_BookAccept value) bookAccept,
    required TResult Function(ActionRequest_BookDismiss value) bookDismiss,
    required TResult Function(ActionRequest_InstallMcp value) installMcp,
    required TResult Function(ActionRequest_DisplayPairingApprove value)
        displayPairingApprove,
    required TResult Function(ActionRequest_DisplayPairingReject value)
        displayPairingReject,
    required TResult Function(ActionRequest_DisplayAssignmentPut value)
        displayAssignmentPut,
    required TResult Function(ActionRequest_DisplayAssignmentRevoke value)
        displayAssignmentRevoke,
    required TResult Function(ActionRequest_DisplayDeviceRevoke value)
        displayDeviceRevoke,
    required TResult Function(
            ActionRequest_DisplayIdentifierAdmitPassphrase value)
        displayIdentifierAdmitPassphrase,
    required TResult Function(ActionRequest_EnterPresentation value)
        enterPresentation,
    required TResult Function(ActionRequest_PresentHere value) presentHere,
    required TResult Function(ActionRequest_PresentRefresh value)
        presentRefresh,
    required TResult Function(ActionRequest_LeavePresentation value)
        leavePresentation,
  }) {
    final _that = this;
    switch (_that) {
      case ActionRequest_Refresh():
        return refresh(_that);
      case ActionRequest_Open():
        return open(_that);
      case ActionRequest_StartDevice():
        return startDevice(_that);
      case ActionRequest_StopDevice():
        return stopDevice(_that);
      case ActionRequest_RestartDevice():
        return restartDevice(_that);
      case ActionRequest_ForceStopDevice():
        return forceStopDevice(_that);
      case ActionRequest_StopAllOwned():
        return stopAllOwned(_that);
      case ActionRequest_RemoveDevice():
        return removeDevice(_that);
      case ActionRequest_ReadSpace():
        return readSpace(_that);
      case ActionRequest_StartHead():
        return startHead(_that);
      case ActionRequest_StopHead():
        return stopHead(_that);
      case ActionRequest_ForgetOrbit():
        return forgetOrbit(_that);
      case ActionRequest_BookPut():
        return bookPut(_that);
      case ActionRequest_BookDelete():
        return bookDelete(_that);
      case ActionRequest_BookSetPicture():
        return bookSetPicture(_that);
      case ActionRequest_BookMerge():
        return bookMerge(_that);
      case ActionRequest_BookClaimSelf():
        return bookClaimSelf(_that);
      case ActionRequest_BookLink():
        return bookLink(_that);
      case ActionRequest_BookUnlink():
        return bookUnlink(_that);
      case ActionRequest_BookExport():
        return bookExport(_that);
      case ActionRequest_BookImport():
        return bookImport(_that);
      case ActionRequest_BookAccept():
        return bookAccept(_that);
      case ActionRequest_BookDismiss():
        return bookDismiss(_that);
      case ActionRequest_InstallMcp():
        return installMcp(_that);
      case ActionRequest_DisplayPairingApprove():
        return displayPairingApprove(_that);
      case ActionRequest_DisplayPairingReject():
        return displayPairingReject(_that);
      case ActionRequest_DisplayAssignmentPut():
        return displayAssignmentPut(_that);
      case ActionRequest_DisplayAssignmentRevoke():
        return displayAssignmentRevoke(_that);
      case ActionRequest_DisplayDeviceRevoke():
        return displayDeviceRevoke(_that);
      case ActionRequest_DisplayIdentifierAdmitPassphrase():
        return displayIdentifierAdmitPassphrase(_that);
      case ActionRequest_EnterPresentation():
        return enterPresentation(_that);
      case ActionRequest_PresentHere():
        return presentHere(_that);
      case ActionRequest_PresentRefresh():
        return presentRefresh(_that);
      case ActionRequest_LeavePresentation():
        return leavePresentation(_that);
    }
  }

  /// A variant of `map` that fallback to returning `null`.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case _:
  ///     return null;
  /// }
  /// ```

  @optionalTypeArgs
  TResult? mapOrNull<TResult extends Object?>({
    TResult? Function(ActionRequest_Refresh value)? refresh,
    TResult? Function(ActionRequest_Open value)? open,
    TResult? Function(ActionRequest_StartDevice value)? startDevice,
    TResult? Function(ActionRequest_StopDevice value)? stopDevice,
    TResult? Function(ActionRequest_RestartDevice value)? restartDevice,
    TResult? Function(ActionRequest_ForceStopDevice value)? forceStopDevice,
    TResult? Function(ActionRequest_StopAllOwned value)? stopAllOwned,
    TResult? Function(ActionRequest_RemoveDevice value)? removeDevice,
    TResult? Function(ActionRequest_ReadSpace value)? readSpace,
    TResult? Function(ActionRequest_StartHead value)? startHead,
    TResult? Function(ActionRequest_StopHead value)? stopHead,
    TResult? Function(ActionRequest_ForgetOrbit value)? forgetOrbit,
    TResult? Function(ActionRequest_BookPut value)? bookPut,
    TResult? Function(ActionRequest_BookDelete value)? bookDelete,
    TResult? Function(ActionRequest_BookSetPicture value)? bookSetPicture,
    TResult? Function(ActionRequest_BookMerge value)? bookMerge,
    TResult? Function(ActionRequest_BookClaimSelf value)? bookClaimSelf,
    TResult? Function(ActionRequest_BookLink value)? bookLink,
    TResult? Function(ActionRequest_BookUnlink value)? bookUnlink,
    TResult? Function(ActionRequest_BookExport value)? bookExport,
    TResult? Function(ActionRequest_BookImport value)? bookImport,
    TResult? Function(ActionRequest_BookAccept value)? bookAccept,
    TResult? Function(ActionRequest_BookDismiss value)? bookDismiss,
    TResult? Function(ActionRequest_InstallMcp value)? installMcp,
    TResult? Function(ActionRequest_DisplayPairingApprove value)?
        displayPairingApprove,
    TResult? Function(ActionRequest_DisplayPairingReject value)?
        displayPairingReject,
    TResult? Function(ActionRequest_DisplayAssignmentPut value)?
        displayAssignmentPut,
    TResult? Function(ActionRequest_DisplayAssignmentRevoke value)?
        displayAssignmentRevoke,
    TResult? Function(ActionRequest_DisplayDeviceRevoke value)?
        displayDeviceRevoke,
    TResult? Function(ActionRequest_DisplayIdentifierAdmitPassphrase value)?
        displayIdentifierAdmitPassphrase,
    TResult? Function(ActionRequest_EnterPresentation value)? enterPresentation,
    TResult? Function(ActionRequest_PresentHere value)? presentHere,
    TResult? Function(ActionRequest_PresentRefresh value)? presentRefresh,
    TResult? Function(ActionRequest_LeavePresentation value)? leavePresentation,
  }) {
    final _that = this;
    switch (_that) {
      case ActionRequest_Refresh() when refresh != null:
        return refresh(_that);
      case ActionRequest_Open() when open != null:
        return open(_that);
      case ActionRequest_StartDevice() when startDevice != null:
        return startDevice(_that);
      case ActionRequest_StopDevice() when stopDevice != null:
        return stopDevice(_that);
      case ActionRequest_RestartDevice() when restartDevice != null:
        return restartDevice(_that);
      case ActionRequest_ForceStopDevice() when forceStopDevice != null:
        return forceStopDevice(_that);
      case ActionRequest_StopAllOwned() when stopAllOwned != null:
        return stopAllOwned(_that);
      case ActionRequest_RemoveDevice() when removeDevice != null:
        return removeDevice(_that);
      case ActionRequest_ReadSpace() when readSpace != null:
        return readSpace(_that);
      case ActionRequest_StartHead() when startHead != null:
        return startHead(_that);
      case ActionRequest_StopHead() when stopHead != null:
        return stopHead(_that);
      case ActionRequest_ForgetOrbit() when forgetOrbit != null:
        return forgetOrbit(_that);
      case ActionRequest_BookPut() when bookPut != null:
        return bookPut(_that);
      case ActionRequest_BookDelete() when bookDelete != null:
        return bookDelete(_that);
      case ActionRequest_BookSetPicture() when bookSetPicture != null:
        return bookSetPicture(_that);
      case ActionRequest_BookMerge() when bookMerge != null:
        return bookMerge(_that);
      case ActionRequest_BookClaimSelf() when bookClaimSelf != null:
        return bookClaimSelf(_that);
      case ActionRequest_BookLink() when bookLink != null:
        return bookLink(_that);
      case ActionRequest_BookUnlink() when bookUnlink != null:
        return bookUnlink(_that);
      case ActionRequest_BookExport() when bookExport != null:
        return bookExport(_that);
      case ActionRequest_BookImport() when bookImport != null:
        return bookImport(_that);
      case ActionRequest_BookAccept() when bookAccept != null:
        return bookAccept(_that);
      case ActionRequest_BookDismiss() when bookDismiss != null:
        return bookDismiss(_that);
      case ActionRequest_InstallMcp() when installMcp != null:
        return installMcp(_that);
      case ActionRequest_DisplayPairingApprove()
          when displayPairingApprove != null:
        return displayPairingApprove(_that);
      case ActionRequest_DisplayPairingReject()
          when displayPairingReject != null:
        return displayPairingReject(_that);
      case ActionRequest_DisplayAssignmentPut()
          when displayAssignmentPut != null:
        return displayAssignmentPut(_that);
      case ActionRequest_DisplayAssignmentRevoke()
          when displayAssignmentRevoke != null:
        return displayAssignmentRevoke(_that);
      case ActionRequest_DisplayDeviceRevoke() when displayDeviceRevoke != null:
        return displayDeviceRevoke(_that);
      case ActionRequest_DisplayIdentifierAdmitPassphrase()
          when displayIdentifierAdmitPassphrase != null:
        return displayIdentifierAdmitPassphrase(_that);
      case ActionRequest_EnterPresentation() when enterPresentation != null:
        return enterPresentation(_that);
      case ActionRequest_PresentHere() when presentHere != null:
        return presentHere(_that);
      case ActionRequest_PresentRefresh() when presentRefresh != null:
        return presentRefresh(_that);
      case ActionRequest_LeavePresentation() when leavePresentation != null:
        return leavePresentation(_that);
      case _:
        return null;
    }
  }

  /// A variant of `when` that fallback to an `orElse` callback.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case _:
  ///     return orElse();
  /// }
  /// ```

  @optionalTypeArgs
  TResult maybeWhen<TResult extends Object?>({
    TResult Function()? refresh,
    TResult Function(String entryPath)? open,
    TResult Function(String id)? startDevice,
    TResult Function(String id)? stopDevice,
    TResult Function(String id)? restartDevice,
    TResult Function(String id)? forceStopDevice,
    TResult Function()? stopAllOwned,
    TResult Function(String id, bool deleteData)? removeDevice,
    TResult Function(String orbit)? readSpace,
    TResult Function()? startHead,
    TResult Function(String id)? stopHead,
    TResult Function(String space)? forgetOrbit,
    TResult Function(String? card, String name, String? note)? bookPut,
    TResult Function(String card)? bookDelete,
    TResult Function(String card, String? path)? bookSetPicture,
    TResult Function(String from, String into)? bookMerge,
    TResult Function(String card)? bookClaimSelf,
    TResult Function(String card, String handle)? bookLink,
    TResult Function(String card, String handle)? bookUnlink,
    TResult Function(String path, List<String>? cards)? bookExport,
    TResult Function(String path)? bookImport,
    TResult Function(String suggestion)? bookAccept,
    TResult Function(String suggestion)? bookDismiss,
    TResult Function(String client, String? scope, String name, String? agent,
            bool noAgent, String project, String? world, bool preview)?
        installMcp,
    TResult Function(String pairing, String label)? displayPairingApprove,
    TResult Function(String pairing)? displayPairingReject,
    TResult Function(
            String device,
            String orbit,
            String world,
            String surface,
            String inputJson,
            DisplayTheme theme,
            int staleAfterMs,
            DisplayStaleAction onStale,
            String? syncGroup,
            DisplaySyncMode syncMode,
            int staticDelayMs,
            BigInt? expiresAtUnixMs)?
        displayAssignmentPut,
    TResult Function(String assignment)? displayAssignmentRevoke,
    TResult Function(String device)? displayDeviceRevoke,
    TResult Function(String passphrase)? displayIdentifierAdmitPassphrase,
    TResult Function()? enterPresentation,
    TResult Function(String orbit, String world, String surface, String input,
            String title)?
        presentHere,
    TResult Function()? presentRefresh,
    TResult Function()? leavePresentation,
    required TResult orElse(),
  }) {
    final _that = this;
    switch (_that) {
      case ActionRequest_Refresh() when refresh != null:
        return refresh();
      case ActionRequest_Open() when open != null:
        return open(_that.entryPath);
      case ActionRequest_StartDevice() when startDevice != null:
        return startDevice(_that.id);
      case ActionRequest_StopDevice() when stopDevice != null:
        return stopDevice(_that.id);
      case ActionRequest_RestartDevice() when restartDevice != null:
        return restartDevice(_that.id);
      case ActionRequest_ForceStopDevice() when forceStopDevice != null:
        return forceStopDevice(_that.id);
      case ActionRequest_StopAllOwned() when stopAllOwned != null:
        return stopAllOwned();
      case ActionRequest_RemoveDevice() when removeDevice != null:
        return removeDevice(_that.id, _that.deleteData);
      case ActionRequest_ReadSpace() when readSpace != null:
        return readSpace(_that.orbit);
      case ActionRequest_StartHead() when startHead != null:
        return startHead();
      case ActionRequest_StopHead() when stopHead != null:
        return stopHead(_that.id);
      case ActionRequest_ForgetOrbit() when forgetOrbit != null:
        return forgetOrbit(_that.space);
      case ActionRequest_BookPut() when bookPut != null:
        return bookPut(_that.card, _that.name, _that.note);
      case ActionRequest_BookDelete() when bookDelete != null:
        return bookDelete(_that.card);
      case ActionRequest_BookSetPicture() when bookSetPicture != null:
        return bookSetPicture(_that.card, _that.path);
      case ActionRequest_BookMerge() when bookMerge != null:
        return bookMerge(_that.from, _that.into);
      case ActionRequest_BookClaimSelf() when bookClaimSelf != null:
        return bookClaimSelf(_that.card);
      case ActionRequest_BookLink() when bookLink != null:
        return bookLink(_that.card, _that.handle);
      case ActionRequest_BookUnlink() when bookUnlink != null:
        return bookUnlink(_that.card, _that.handle);
      case ActionRequest_BookExport() when bookExport != null:
        return bookExport(_that.path, _that.cards);
      case ActionRequest_BookImport() when bookImport != null:
        return bookImport(_that.path);
      case ActionRequest_BookAccept() when bookAccept != null:
        return bookAccept(_that.suggestion);
      case ActionRequest_BookDismiss() when bookDismiss != null:
        return bookDismiss(_that.suggestion);
      case ActionRequest_InstallMcp() when installMcp != null:
        return installMcp(_that.client, _that.scope, _that.name, _that.agent,
            _that.noAgent, _that.project, _that.world, _that.preview);
      case ActionRequest_DisplayPairingApprove()
          when displayPairingApprove != null:
        return displayPairingApprove(_that.pairing, _that.label);
      case ActionRequest_DisplayPairingReject()
          when displayPairingReject != null:
        return displayPairingReject(_that.pairing);
      case ActionRequest_DisplayAssignmentPut()
          when displayAssignmentPut != null:
        return displayAssignmentPut(
            _that.device,
            _that.orbit,
            _that.world,
            _that.surface,
            _that.inputJson,
            _that.theme,
            _that.staleAfterMs,
            _that.onStale,
            _that.syncGroup,
            _that.syncMode,
            _that.staticDelayMs,
            _that.expiresAtUnixMs);
      case ActionRequest_DisplayAssignmentRevoke()
          when displayAssignmentRevoke != null:
        return displayAssignmentRevoke(_that.assignment);
      case ActionRequest_DisplayDeviceRevoke() when displayDeviceRevoke != null:
        return displayDeviceRevoke(_that.device);
      case ActionRequest_DisplayIdentifierAdmitPassphrase()
          when displayIdentifierAdmitPassphrase != null:
        return displayIdentifierAdmitPassphrase(_that.passphrase);
      case ActionRequest_EnterPresentation() when enterPresentation != null:
        return enterPresentation();
      case ActionRequest_PresentHere() when presentHere != null:
        return presentHere(
            _that.orbit, _that.world, _that.surface, _that.input, _that.title);
      case ActionRequest_PresentRefresh() when presentRefresh != null:
        return presentRefresh();
      case ActionRequest_LeavePresentation() when leavePresentation != null:
        return leavePresentation();
      case _:
        return orElse();
    }
  }

  /// A `switch`-like method, using callbacks.
  ///
  /// As opposed to `map`, this offers destructuring.
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case Subclass2(:final field2):
  ///     return ...;
  /// }
  /// ```

  @optionalTypeArgs
  TResult when<TResult extends Object?>({
    required TResult Function() refresh,
    required TResult Function(String entryPath) open,
    required TResult Function(String id) startDevice,
    required TResult Function(String id) stopDevice,
    required TResult Function(String id) restartDevice,
    required TResult Function(String id) forceStopDevice,
    required TResult Function() stopAllOwned,
    required TResult Function(String id, bool deleteData) removeDevice,
    required TResult Function(String orbit) readSpace,
    required TResult Function() startHead,
    required TResult Function(String id) stopHead,
    required TResult Function(String space) forgetOrbit,
    required TResult Function(String? card, String name, String? note) bookPut,
    required TResult Function(String card) bookDelete,
    required TResult Function(String card, String? path) bookSetPicture,
    required TResult Function(String from, String into) bookMerge,
    required TResult Function(String card) bookClaimSelf,
    required TResult Function(String card, String handle) bookLink,
    required TResult Function(String card, String handle) bookUnlink,
    required TResult Function(String path, List<String>? cards) bookExport,
    required TResult Function(String path) bookImport,
    required TResult Function(String suggestion) bookAccept,
    required TResult Function(String suggestion) bookDismiss,
    required TResult Function(
            String client,
            String? scope,
            String name,
            String? agent,
            bool noAgent,
            String project,
            String? world,
            bool preview)
        installMcp,
    required TResult Function(String pairing, String label)
        displayPairingApprove,
    required TResult Function(String pairing) displayPairingReject,
    required TResult Function(
            String device,
            String orbit,
            String world,
            String surface,
            String inputJson,
            DisplayTheme theme,
            int staleAfterMs,
            DisplayStaleAction onStale,
            String? syncGroup,
            DisplaySyncMode syncMode,
            int staticDelayMs,
            BigInt? expiresAtUnixMs)
        displayAssignmentPut,
    required TResult Function(String assignment) displayAssignmentRevoke,
    required TResult Function(String device) displayDeviceRevoke,
    required TResult Function(String passphrase)
        displayIdentifierAdmitPassphrase,
    required TResult Function() enterPresentation,
    required TResult Function(String orbit, String world, String surface,
            String input, String title)
        presentHere,
    required TResult Function() presentRefresh,
    required TResult Function() leavePresentation,
  }) {
    final _that = this;
    switch (_that) {
      case ActionRequest_Refresh():
        return refresh();
      case ActionRequest_Open():
        return open(_that.entryPath);
      case ActionRequest_StartDevice():
        return startDevice(_that.id);
      case ActionRequest_StopDevice():
        return stopDevice(_that.id);
      case ActionRequest_RestartDevice():
        return restartDevice(_that.id);
      case ActionRequest_ForceStopDevice():
        return forceStopDevice(_that.id);
      case ActionRequest_StopAllOwned():
        return stopAllOwned();
      case ActionRequest_RemoveDevice():
        return removeDevice(_that.id, _that.deleteData);
      case ActionRequest_ReadSpace():
        return readSpace(_that.orbit);
      case ActionRequest_StartHead():
        return startHead();
      case ActionRequest_StopHead():
        return stopHead(_that.id);
      case ActionRequest_ForgetOrbit():
        return forgetOrbit(_that.space);
      case ActionRequest_BookPut():
        return bookPut(_that.card, _that.name, _that.note);
      case ActionRequest_BookDelete():
        return bookDelete(_that.card);
      case ActionRequest_BookSetPicture():
        return bookSetPicture(_that.card, _that.path);
      case ActionRequest_BookMerge():
        return bookMerge(_that.from, _that.into);
      case ActionRequest_BookClaimSelf():
        return bookClaimSelf(_that.card);
      case ActionRequest_BookLink():
        return bookLink(_that.card, _that.handle);
      case ActionRequest_BookUnlink():
        return bookUnlink(_that.card, _that.handle);
      case ActionRequest_BookExport():
        return bookExport(_that.path, _that.cards);
      case ActionRequest_BookImport():
        return bookImport(_that.path);
      case ActionRequest_BookAccept():
        return bookAccept(_that.suggestion);
      case ActionRequest_BookDismiss():
        return bookDismiss(_that.suggestion);
      case ActionRequest_InstallMcp():
        return installMcp(_that.client, _that.scope, _that.name, _that.agent,
            _that.noAgent, _that.project, _that.world, _that.preview);
      case ActionRequest_DisplayPairingApprove():
        return displayPairingApprove(_that.pairing, _that.label);
      case ActionRequest_DisplayPairingReject():
        return displayPairingReject(_that.pairing);
      case ActionRequest_DisplayAssignmentPut():
        return displayAssignmentPut(
            _that.device,
            _that.orbit,
            _that.world,
            _that.surface,
            _that.inputJson,
            _that.theme,
            _that.staleAfterMs,
            _that.onStale,
            _that.syncGroup,
            _that.syncMode,
            _that.staticDelayMs,
            _that.expiresAtUnixMs);
      case ActionRequest_DisplayAssignmentRevoke():
        return displayAssignmentRevoke(_that.assignment);
      case ActionRequest_DisplayDeviceRevoke():
        return displayDeviceRevoke(_that.device);
      case ActionRequest_DisplayIdentifierAdmitPassphrase():
        return displayIdentifierAdmitPassphrase(_that.passphrase);
      case ActionRequest_EnterPresentation():
        return enterPresentation();
      case ActionRequest_PresentHere():
        return presentHere(
            _that.orbit, _that.world, _that.surface, _that.input, _that.title);
      case ActionRequest_PresentRefresh():
        return presentRefresh();
      case ActionRequest_LeavePresentation():
        return leavePresentation();
    }
  }

  /// A variant of `when` that fallback to returning `null`
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case _:
  ///     return null;
  /// }
  /// ```

  @optionalTypeArgs
  TResult? whenOrNull<TResult extends Object?>({
    TResult? Function()? refresh,
    TResult? Function(String entryPath)? open,
    TResult? Function(String id)? startDevice,
    TResult? Function(String id)? stopDevice,
    TResult? Function(String id)? restartDevice,
    TResult? Function(String id)? forceStopDevice,
    TResult? Function()? stopAllOwned,
    TResult? Function(String id, bool deleteData)? removeDevice,
    TResult? Function(String orbit)? readSpace,
    TResult? Function()? startHead,
    TResult? Function(String id)? stopHead,
    TResult? Function(String space)? forgetOrbit,
    TResult? Function(String? card, String name, String? note)? bookPut,
    TResult? Function(String card)? bookDelete,
    TResult? Function(String card, String? path)? bookSetPicture,
    TResult? Function(String from, String into)? bookMerge,
    TResult? Function(String card)? bookClaimSelf,
    TResult? Function(String card, String handle)? bookLink,
    TResult? Function(String card, String handle)? bookUnlink,
    TResult? Function(String path, List<String>? cards)? bookExport,
    TResult? Function(String path)? bookImport,
    TResult? Function(String suggestion)? bookAccept,
    TResult? Function(String suggestion)? bookDismiss,
    TResult? Function(String client, String? scope, String name, String? agent,
            bool noAgent, String project, String? world, bool preview)?
        installMcp,
    TResult? Function(String pairing, String label)? displayPairingApprove,
    TResult? Function(String pairing)? displayPairingReject,
    TResult? Function(
            String device,
            String orbit,
            String world,
            String surface,
            String inputJson,
            DisplayTheme theme,
            int staleAfterMs,
            DisplayStaleAction onStale,
            String? syncGroup,
            DisplaySyncMode syncMode,
            int staticDelayMs,
            BigInt? expiresAtUnixMs)?
        displayAssignmentPut,
    TResult? Function(String assignment)? displayAssignmentRevoke,
    TResult? Function(String device)? displayDeviceRevoke,
    TResult? Function(String passphrase)? displayIdentifierAdmitPassphrase,
    TResult? Function()? enterPresentation,
    TResult? Function(String orbit, String world, String surface, String input,
            String title)?
        presentHere,
    TResult? Function()? presentRefresh,
    TResult? Function()? leavePresentation,
  }) {
    final _that = this;
    switch (_that) {
      case ActionRequest_Refresh() when refresh != null:
        return refresh();
      case ActionRequest_Open() when open != null:
        return open(_that.entryPath);
      case ActionRequest_StartDevice() when startDevice != null:
        return startDevice(_that.id);
      case ActionRequest_StopDevice() when stopDevice != null:
        return stopDevice(_that.id);
      case ActionRequest_RestartDevice() when restartDevice != null:
        return restartDevice(_that.id);
      case ActionRequest_ForceStopDevice() when forceStopDevice != null:
        return forceStopDevice(_that.id);
      case ActionRequest_StopAllOwned() when stopAllOwned != null:
        return stopAllOwned();
      case ActionRequest_RemoveDevice() when removeDevice != null:
        return removeDevice(_that.id, _that.deleteData);
      case ActionRequest_ReadSpace() when readSpace != null:
        return readSpace(_that.orbit);
      case ActionRequest_StartHead() when startHead != null:
        return startHead();
      case ActionRequest_StopHead() when stopHead != null:
        return stopHead(_that.id);
      case ActionRequest_ForgetOrbit() when forgetOrbit != null:
        return forgetOrbit(_that.space);
      case ActionRequest_BookPut() when bookPut != null:
        return bookPut(_that.card, _that.name, _that.note);
      case ActionRequest_BookDelete() when bookDelete != null:
        return bookDelete(_that.card);
      case ActionRequest_BookSetPicture() when bookSetPicture != null:
        return bookSetPicture(_that.card, _that.path);
      case ActionRequest_BookMerge() when bookMerge != null:
        return bookMerge(_that.from, _that.into);
      case ActionRequest_BookClaimSelf() when bookClaimSelf != null:
        return bookClaimSelf(_that.card);
      case ActionRequest_BookLink() when bookLink != null:
        return bookLink(_that.card, _that.handle);
      case ActionRequest_BookUnlink() when bookUnlink != null:
        return bookUnlink(_that.card, _that.handle);
      case ActionRequest_BookExport() when bookExport != null:
        return bookExport(_that.path, _that.cards);
      case ActionRequest_BookImport() when bookImport != null:
        return bookImport(_that.path);
      case ActionRequest_BookAccept() when bookAccept != null:
        return bookAccept(_that.suggestion);
      case ActionRequest_BookDismiss() when bookDismiss != null:
        return bookDismiss(_that.suggestion);
      case ActionRequest_InstallMcp() when installMcp != null:
        return installMcp(_that.client, _that.scope, _that.name, _that.agent,
            _that.noAgent, _that.project, _that.world, _that.preview);
      case ActionRequest_DisplayPairingApprove()
          when displayPairingApprove != null:
        return displayPairingApprove(_that.pairing, _that.label);
      case ActionRequest_DisplayPairingReject()
          when displayPairingReject != null:
        return displayPairingReject(_that.pairing);
      case ActionRequest_DisplayAssignmentPut()
          when displayAssignmentPut != null:
        return displayAssignmentPut(
            _that.device,
            _that.orbit,
            _that.world,
            _that.surface,
            _that.inputJson,
            _that.theme,
            _that.staleAfterMs,
            _that.onStale,
            _that.syncGroup,
            _that.syncMode,
            _that.staticDelayMs,
            _that.expiresAtUnixMs);
      case ActionRequest_DisplayAssignmentRevoke()
          when displayAssignmentRevoke != null:
        return displayAssignmentRevoke(_that.assignment);
      case ActionRequest_DisplayDeviceRevoke() when displayDeviceRevoke != null:
        return displayDeviceRevoke(_that.device);
      case ActionRequest_DisplayIdentifierAdmitPassphrase()
          when displayIdentifierAdmitPassphrase != null:
        return displayIdentifierAdmitPassphrase(_that.passphrase);
      case ActionRequest_EnterPresentation() when enterPresentation != null:
        return enterPresentation();
      case ActionRequest_PresentHere() when presentHere != null:
        return presentHere(
            _that.orbit, _that.world, _that.surface, _that.input, _that.title);
      case ActionRequest_PresentRefresh() when presentRefresh != null:
        return presentRefresh();
      case ActionRequest_LeavePresentation() when leavePresentation != null:
        return leavePresentation();
      case _:
        return null;
    }
  }
}

/// @nodoc

class ActionRequest_Refresh extends ActionRequest {
  const ActionRequest_Refresh() : super._();

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType && other is ActionRequest_Refresh);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'ActionRequest.refresh()';
  }
}

/// @nodoc

class ActionRequest_Open extends ActionRequest {
  const ActionRequest_Open({required this.entryPath}) : super._();

  final String entryPath;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_OpenCopyWith<ActionRequest_Open> get copyWith =>
      _$ActionRequest_OpenCopyWithImpl<ActionRequest_Open>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_Open &&
            (identical(other.entryPath, entryPath) ||
                other.entryPath == entryPath));
  }

  @override
  int get hashCode => Object.hash(runtimeType, entryPath);

  @override
  String toString() {
    return 'ActionRequest.open(entryPath: $entryPath)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_OpenCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_OpenCopyWith(
          ActionRequest_Open value, $Res Function(ActionRequest_Open) _then) =
      _$ActionRequest_OpenCopyWithImpl;
  @useResult
  $Res call({String entryPath});
}

/// @nodoc
class _$ActionRequest_OpenCopyWithImpl<$Res>
    implements $ActionRequest_OpenCopyWith<$Res> {
  _$ActionRequest_OpenCopyWithImpl(this._self, this._then);

  final ActionRequest_Open _self;
  final $Res Function(ActionRequest_Open) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? entryPath = null,
  }) {
    return _then(ActionRequest_Open(
      entryPath: null == entryPath
          ? _self.entryPath
          : entryPath // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_StartDevice extends ActionRequest {
  const ActionRequest_StartDevice({required this.id}) : super._();

  final String id;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_StartDeviceCopyWith<ActionRequest_StartDevice> get copyWith =>
      _$ActionRequest_StartDeviceCopyWithImpl<ActionRequest_StartDevice>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_StartDevice &&
            (identical(other.id, id) || other.id == id));
  }

  @override
  int get hashCode => Object.hash(runtimeType, id);

  @override
  String toString() {
    return 'ActionRequest.startDevice(id: $id)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_StartDeviceCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_StartDeviceCopyWith(ActionRequest_StartDevice value,
          $Res Function(ActionRequest_StartDevice) _then) =
      _$ActionRequest_StartDeviceCopyWithImpl;
  @useResult
  $Res call({String id});
}

/// @nodoc
class _$ActionRequest_StartDeviceCopyWithImpl<$Res>
    implements $ActionRequest_StartDeviceCopyWith<$Res> {
  _$ActionRequest_StartDeviceCopyWithImpl(this._self, this._then);

  final ActionRequest_StartDevice _self;
  final $Res Function(ActionRequest_StartDevice) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? id = null,
  }) {
    return _then(ActionRequest_StartDevice(
      id: null == id
          ? _self.id
          : id // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_StopDevice extends ActionRequest {
  const ActionRequest_StopDevice({required this.id}) : super._();

  final String id;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_StopDeviceCopyWith<ActionRequest_StopDevice> get copyWith =>
      _$ActionRequest_StopDeviceCopyWithImpl<ActionRequest_StopDevice>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_StopDevice &&
            (identical(other.id, id) || other.id == id));
  }

  @override
  int get hashCode => Object.hash(runtimeType, id);

  @override
  String toString() {
    return 'ActionRequest.stopDevice(id: $id)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_StopDeviceCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_StopDeviceCopyWith(ActionRequest_StopDevice value,
          $Res Function(ActionRequest_StopDevice) _then) =
      _$ActionRequest_StopDeviceCopyWithImpl;
  @useResult
  $Res call({String id});
}

/// @nodoc
class _$ActionRequest_StopDeviceCopyWithImpl<$Res>
    implements $ActionRequest_StopDeviceCopyWith<$Res> {
  _$ActionRequest_StopDeviceCopyWithImpl(this._self, this._then);

  final ActionRequest_StopDevice _self;
  final $Res Function(ActionRequest_StopDevice) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? id = null,
  }) {
    return _then(ActionRequest_StopDevice(
      id: null == id
          ? _self.id
          : id // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_RestartDevice extends ActionRequest {
  const ActionRequest_RestartDevice({required this.id}) : super._();

  final String id;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_RestartDeviceCopyWith<ActionRequest_RestartDevice>
      get copyWith => _$ActionRequest_RestartDeviceCopyWithImpl<
          ActionRequest_RestartDevice>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_RestartDevice &&
            (identical(other.id, id) || other.id == id));
  }

  @override
  int get hashCode => Object.hash(runtimeType, id);

  @override
  String toString() {
    return 'ActionRequest.restartDevice(id: $id)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_RestartDeviceCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_RestartDeviceCopyWith(
          ActionRequest_RestartDevice value,
          $Res Function(ActionRequest_RestartDevice) _then) =
      _$ActionRequest_RestartDeviceCopyWithImpl;
  @useResult
  $Res call({String id});
}

/// @nodoc
class _$ActionRequest_RestartDeviceCopyWithImpl<$Res>
    implements $ActionRequest_RestartDeviceCopyWith<$Res> {
  _$ActionRequest_RestartDeviceCopyWithImpl(this._self, this._then);

  final ActionRequest_RestartDevice _self;
  final $Res Function(ActionRequest_RestartDevice) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? id = null,
  }) {
    return _then(ActionRequest_RestartDevice(
      id: null == id
          ? _self.id
          : id // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_ForceStopDevice extends ActionRequest {
  const ActionRequest_ForceStopDevice({required this.id}) : super._();

  final String id;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_ForceStopDeviceCopyWith<ActionRequest_ForceStopDevice>
      get copyWith => _$ActionRequest_ForceStopDeviceCopyWithImpl<
          ActionRequest_ForceStopDevice>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_ForceStopDevice &&
            (identical(other.id, id) || other.id == id));
  }

  @override
  int get hashCode => Object.hash(runtimeType, id);

  @override
  String toString() {
    return 'ActionRequest.forceStopDevice(id: $id)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_ForceStopDeviceCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_ForceStopDeviceCopyWith(
          ActionRequest_ForceStopDevice value,
          $Res Function(ActionRequest_ForceStopDevice) _then) =
      _$ActionRequest_ForceStopDeviceCopyWithImpl;
  @useResult
  $Res call({String id});
}

/// @nodoc
class _$ActionRequest_ForceStopDeviceCopyWithImpl<$Res>
    implements $ActionRequest_ForceStopDeviceCopyWith<$Res> {
  _$ActionRequest_ForceStopDeviceCopyWithImpl(this._self, this._then);

  final ActionRequest_ForceStopDevice _self;
  final $Res Function(ActionRequest_ForceStopDevice) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? id = null,
  }) {
    return _then(ActionRequest_ForceStopDevice(
      id: null == id
          ? _self.id
          : id // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_StopAllOwned extends ActionRequest {
  const ActionRequest_StopAllOwned() : super._();

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_StopAllOwned);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'ActionRequest.stopAllOwned()';
  }
}

/// @nodoc

class ActionRequest_RemoveDevice extends ActionRequest {
  const ActionRequest_RemoveDevice({required this.id, required this.deleteData})
      : super._();

  final String id;
  final bool deleteData;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_RemoveDeviceCopyWith<ActionRequest_RemoveDevice>
      get copyWith =>
          _$ActionRequest_RemoveDeviceCopyWithImpl<ActionRequest_RemoveDevice>(
              this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_RemoveDevice &&
            (identical(other.id, id) || other.id == id) &&
            (identical(other.deleteData, deleteData) ||
                other.deleteData == deleteData));
  }

  @override
  int get hashCode => Object.hash(runtimeType, id, deleteData);

  @override
  String toString() {
    return 'ActionRequest.removeDevice(id: $id, deleteData: $deleteData)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_RemoveDeviceCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_RemoveDeviceCopyWith(ActionRequest_RemoveDevice value,
          $Res Function(ActionRequest_RemoveDevice) _then) =
      _$ActionRequest_RemoveDeviceCopyWithImpl;
  @useResult
  $Res call({String id, bool deleteData});
}

/// @nodoc
class _$ActionRequest_RemoveDeviceCopyWithImpl<$Res>
    implements $ActionRequest_RemoveDeviceCopyWith<$Res> {
  _$ActionRequest_RemoveDeviceCopyWithImpl(this._self, this._then);

  final ActionRequest_RemoveDevice _self;
  final $Res Function(ActionRequest_RemoveDevice) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? id = null,
    Object? deleteData = null,
  }) {
    return _then(ActionRequest_RemoveDevice(
      id: null == id
          ? _self.id
          : id // ignore: cast_nullable_to_non_nullable
              as String,
      deleteData: null == deleteData
          ? _self.deleteData
          : deleteData // ignore: cast_nullable_to_non_nullable
              as bool,
    ));
  }
}

/// @nodoc

class ActionRequest_ReadSpace extends ActionRequest {
  const ActionRequest_ReadSpace({required this.orbit}) : super._();

  final String orbit;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_ReadSpaceCopyWith<ActionRequest_ReadSpace> get copyWith =>
      _$ActionRequest_ReadSpaceCopyWithImpl<ActionRequest_ReadSpace>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_ReadSpace &&
            (identical(other.orbit, orbit) || other.orbit == orbit));
  }

  @override
  int get hashCode => Object.hash(runtimeType, orbit);

  @override
  String toString() {
    return 'ActionRequest.readSpace(orbit: $orbit)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_ReadSpaceCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_ReadSpaceCopyWith(ActionRequest_ReadSpace value,
          $Res Function(ActionRequest_ReadSpace) _then) =
      _$ActionRequest_ReadSpaceCopyWithImpl;
  @useResult
  $Res call({String orbit});
}

/// @nodoc
class _$ActionRequest_ReadSpaceCopyWithImpl<$Res>
    implements $ActionRequest_ReadSpaceCopyWith<$Res> {
  _$ActionRequest_ReadSpaceCopyWithImpl(this._self, this._then);

  final ActionRequest_ReadSpace _self;
  final $Res Function(ActionRequest_ReadSpace) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? orbit = null,
  }) {
    return _then(ActionRequest_ReadSpace(
      orbit: null == orbit
          ? _self.orbit
          : orbit // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_StartHead extends ActionRequest {
  const ActionRequest_StartHead() : super._();

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType && other is ActionRequest_StartHead);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'ActionRequest.startHead()';
  }
}

/// @nodoc

class ActionRequest_StopHead extends ActionRequest {
  const ActionRequest_StopHead({required this.id}) : super._();

  final String id;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_StopHeadCopyWith<ActionRequest_StopHead> get copyWith =>
      _$ActionRequest_StopHeadCopyWithImpl<ActionRequest_StopHead>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_StopHead &&
            (identical(other.id, id) || other.id == id));
  }

  @override
  int get hashCode => Object.hash(runtimeType, id);

  @override
  String toString() {
    return 'ActionRequest.stopHead(id: $id)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_StopHeadCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_StopHeadCopyWith(ActionRequest_StopHead value,
          $Res Function(ActionRequest_StopHead) _then) =
      _$ActionRequest_StopHeadCopyWithImpl;
  @useResult
  $Res call({String id});
}

/// @nodoc
class _$ActionRequest_StopHeadCopyWithImpl<$Res>
    implements $ActionRequest_StopHeadCopyWith<$Res> {
  _$ActionRequest_StopHeadCopyWithImpl(this._self, this._then);

  final ActionRequest_StopHead _self;
  final $Res Function(ActionRequest_StopHead) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? id = null,
  }) {
    return _then(ActionRequest_StopHead(
      id: null == id
          ? _self.id
          : id // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_ForgetOrbit extends ActionRequest {
  const ActionRequest_ForgetOrbit({required this.space}) : super._();

  final String space;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_ForgetOrbitCopyWith<ActionRequest_ForgetOrbit> get copyWith =>
      _$ActionRequest_ForgetOrbitCopyWithImpl<ActionRequest_ForgetOrbit>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_ForgetOrbit &&
            (identical(other.space, space) || other.space == space));
  }

  @override
  int get hashCode => Object.hash(runtimeType, space);

  @override
  String toString() {
    return 'ActionRequest.forgetOrbit(space: $space)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_ForgetOrbitCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_ForgetOrbitCopyWith(ActionRequest_ForgetOrbit value,
          $Res Function(ActionRequest_ForgetOrbit) _then) =
      _$ActionRequest_ForgetOrbitCopyWithImpl;
  @useResult
  $Res call({String space});
}

/// @nodoc
class _$ActionRequest_ForgetOrbitCopyWithImpl<$Res>
    implements $ActionRequest_ForgetOrbitCopyWith<$Res> {
  _$ActionRequest_ForgetOrbitCopyWithImpl(this._self, this._then);

  final ActionRequest_ForgetOrbit _self;
  final $Res Function(ActionRequest_ForgetOrbit) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? space = null,
  }) {
    return _then(ActionRequest_ForgetOrbit(
      space: null == space
          ? _self.space
          : space // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_BookPut extends ActionRequest {
  const ActionRequest_BookPut({this.card, required this.name, this.note})
      : super._();

  final String? card;
  final String name;
  final String? note;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookPutCopyWith<ActionRequest_BookPut> get copyWith =>
      _$ActionRequest_BookPutCopyWithImpl<ActionRequest_BookPut>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookPut &&
            (identical(other.card, card) || other.card == card) &&
            (identical(other.name, name) || other.name == name) &&
            (identical(other.note, note) || other.note == note));
  }

  @override
  int get hashCode => Object.hash(runtimeType, card, name, note);

  @override
  String toString() {
    return 'ActionRequest.bookPut(card: $card, name: $name, note: $note)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookPutCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookPutCopyWith(ActionRequest_BookPut value,
          $Res Function(ActionRequest_BookPut) _then) =
      _$ActionRequest_BookPutCopyWithImpl;
  @useResult
  $Res call({String? card, String name, String? note});
}

/// @nodoc
class _$ActionRequest_BookPutCopyWithImpl<$Res>
    implements $ActionRequest_BookPutCopyWith<$Res> {
  _$ActionRequest_BookPutCopyWithImpl(this._self, this._then);

  final ActionRequest_BookPut _self;
  final $Res Function(ActionRequest_BookPut) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? card = freezed,
    Object? name = null,
    Object? note = freezed,
  }) {
    return _then(ActionRequest_BookPut(
      card: freezed == card
          ? _self.card
          : card // ignore: cast_nullable_to_non_nullable
              as String?,
      name: null == name
          ? _self.name
          : name // ignore: cast_nullable_to_non_nullable
              as String,
      note: freezed == note
          ? _self.note
          : note // ignore: cast_nullable_to_non_nullable
              as String?,
    ));
  }
}

/// @nodoc

class ActionRequest_BookDelete extends ActionRequest {
  const ActionRequest_BookDelete({required this.card}) : super._();

  final String card;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookDeleteCopyWith<ActionRequest_BookDelete> get copyWith =>
      _$ActionRequest_BookDeleteCopyWithImpl<ActionRequest_BookDelete>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookDelete &&
            (identical(other.card, card) || other.card == card));
  }

  @override
  int get hashCode => Object.hash(runtimeType, card);

  @override
  String toString() {
    return 'ActionRequest.bookDelete(card: $card)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookDeleteCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookDeleteCopyWith(ActionRequest_BookDelete value,
          $Res Function(ActionRequest_BookDelete) _then) =
      _$ActionRequest_BookDeleteCopyWithImpl;
  @useResult
  $Res call({String card});
}

/// @nodoc
class _$ActionRequest_BookDeleteCopyWithImpl<$Res>
    implements $ActionRequest_BookDeleteCopyWith<$Res> {
  _$ActionRequest_BookDeleteCopyWithImpl(this._self, this._then);

  final ActionRequest_BookDelete _self;
  final $Res Function(ActionRequest_BookDelete) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? card = null,
  }) {
    return _then(ActionRequest_BookDelete(
      card: null == card
          ? _self.card
          : card // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_BookSetPicture extends ActionRequest {
  const ActionRequest_BookSetPicture({required this.card, this.path})
      : super._();

  final String card;
  final String? path;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookSetPictureCopyWith<ActionRequest_BookSetPicture>
      get copyWith => _$ActionRequest_BookSetPictureCopyWithImpl<
          ActionRequest_BookSetPicture>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookSetPicture &&
            (identical(other.card, card) || other.card == card) &&
            (identical(other.path, path) || other.path == path));
  }

  @override
  int get hashCode => Object.hash(runtimeType, card, path);

  @override
  String toString() {
    return 'ActionRequest.bookSetPicture(card: $card, path: $path)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookSetPictureCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookSetPictureCopyWith(
          ActionRequest_BookSetPicture value,
          $Res Function(ActionRequest_BookSetPicture) _then) =
      _$ActionRequest_BookSetPictureCopyWithImpl;
  @useResult
  $Res call({String card, String? path});
}

/// @nodoc
class _$ActionRequest_BookSetPictureCopyWithImpl<$Res>
    implements $ActionRequest_BookSetPictureCopyWith<$Res> {
  _$ActionRequest_BookSetPictureCopyWithImpl(this._self, this._then);

  final ActionRequest_BookSetPicture _self;
  final $Res Function(ActionRequest_BookSetPicture) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? card = null,
    Object? path = freezed,
  }) {
    return _then(ActionRequest_BookSetPicture(
      card: null == card
          ? _self.card
          : card // ignore: cast_nullable_to_non_nullable
              as String,
      path: freezed == path
          ? _self.path
          : path // ignore: cast_nullable_to_non_nullable
              as String?,
    ));
  }
}

/// @nodoc

class ActionRequest_BookMerge extends ActionRequest {
  const ActionRequest_BookMerge({required this.from, required this.into})
      : super._();

  final String from;
  final String into;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookMergeCopyWith<ActionRequest_BookMerge> get copyWith =>
      _$ActionRequest_BookMergeCopyWithImpl<ActionRequest_BookMerge>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookMerge &&
            (identical(other.from, from) || other.from == from) &&
            (identical(other.into, into) || other.into == into));
  }

  @override
  int get hashCode => Object.hash(runtimeType, from, into);

  @override
  String toString() {
    return 'ActionRequest.bookMerge(from: $from, into: $into)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookMergeCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookMergeCopyWith(ActionRequest_BookMerge value,
          $Res Function(ActionRequest_BookMerge) _then) =
      _$ActionRequest_BookMergeCopyWithImpl;
  @useResult
  $Res call({String from, String into});
}

/// @nodoc
class _$ActionRequest_BookMergeCopyWithImpl<$Res>
    implements $ActionRequest_BookMergeCopyWith<$Res> {
  _$ActionRequest_BookMergeCopyWithImpl(this._self, this._then);

  final ActionRequest_BookMerge _self;
  final $Res Function(ActionRequest_BookMerge) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? from = null,
    Object? into = null,
  }) {
    return _then(ActionRequest_BookMerge(
      from: null == from
          ? _self.from
          : from // ignore: cast_nullable_to_non_nullable
              as String,
      into: null == into
          ? _self.into
          : into // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_BookClaimSelf extends ActionRequest {
  const ActionRequest_BookClaimSelf({required this.card}) : super._();

  final String card;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookClaimSelfCopyWith<ActionRequest_BookClaimSelf>
      get copyWith => _$ActionRequest_BookClaimSelfCopyWithImpl<
          ActionRequest_BookClaimSelf>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookClaimSelf &&
            (identical(other.card, card) || other.card == card));
  }

  @override
  int get hashCode => Object.hash(runtimeType, card);

  @override
  String toString() {
    return 'ActionRequest.bookClaimSelf(card: $card)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookClaimSelfCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookClaimSelfCopyWith(
          ActionRequest_BookClaimSelf value,
          $Res Function(ActionRequest_BookClaimSelf) _then) =
      _$ActionRequest_BookClaimSelfCopyWithImpl;
  @useResult
  $Res call({String card});
}

/// @nodoc
class _$ActionRequest_BookClaimSelfCopyWithImpl<$Res>
    implements $ActionRequest_BookClaimSelfCopyWith<$Res> {
  _$ActionRequest_BookClaimSelfCopyWithImpl(this._self, this._then);

  final ActionRequest_BookClaimSelf _self;
  final $Res Function(ActionRequest_BookClaimSelf) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? card = null,
  }) {
    return _then(ActionRequest_BookClaimSelf(
      card: null == card
          ? _self.card
          : card // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_BookLink extends ActionRequest {
  const ActionRequest_BookLink({required this.card, required this.handle})
      : super._();

  final String card;
  final String handle;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookLinkCopyWith<ActionRequest_BookLink> get copyWith =>
      _$ActionRequest_BookLinkCopyWithImpl<ActionRequest_BookLink>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookLink &&
            (identical(other.card, card) || other.card == card) &&
            (identical(other.handle, handle) || other.handle == handle));
  }

  @override
  int get hashCode => Object.hash(runtimeType, card, handle);

  @override
  String toString() {
    return 'ActionRequest.bookLink(card: $card, handle: $handle)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookLinkCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookLinkCopyWith(ActionRequest_BookLink value,
          $Res Function(ActionRequest_BookLink) _then) =
      _$ActionRequest_BookLinkCopyWithImpl;
  @useResult
  $Res call({String card, String handle});
}

/// @nodoc
class _$ActionRequest_BookLinkCopyWithImpl<$Res>
    implements $ActionRequest_BookLinkCopyWith<$Res> {
  _$ActionRequest_BookLinkCopyWithImpl(this._self, this._then);

  final ActionRequest_BookLink _self;
  final $Res Function(ActionRequest_BookLink) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? card = null,
    Object? handle = null,
  }) {
    return _then(ActionRequest_BookLink(
      card: null == card
          ? _self.card
          : card // ignore: cast_nullable_to_non_nullable
              as String,
      handle: null == handle
          ? _self.handle
          : handle // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_BookUnlink extends ActionRequest {
  const ActionRequest_BookUnlink({required this.card, required this.handle})
      : super._();

  final String card;
  final String handle;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookUnlinkCopyWith<ActionRequest_BookUnlink> get copyWith =>
      _$ActionRequest_BookUnlinkCopyWithImpl<ActionRequest_BookUnlink>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookUnlink &&
            (identical(other.card, card) || other.card == card) &&
            (identical(other.handle, handle) || other.handle == handle));
  }

  @override
  int get hashCode => Object.hash(runtimeType, card, handle);

  @override
  String toString() {
    return 'ActionRequest.bookUnlink(card: $card, handle: $handle)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookUnlinkCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookUnlinkCopyWith(ActionRequest_BookUnlink value,
          $Res Function(ActionRequest_BookUnlink) _then) =
      _$ActionRequest_BookUnlinkCopyWithImpl;
  @useResult
  $Res call({String card, String handle});
}

/// @nodoc
class _$ActionRequest_BookUnlinkCopyWithImpl<$Res>
    implements $ActionRequest_BookUnlinkCopyWith<$Res> {
  _$ActionRequest_BookUnlinkCopyWithImpl(this._self, this._then);

  final ActionRequest_BookUnlink _self;
  final $Res Function(ActionRequest_BookUnlink) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? card = null,
    Object? handle = null,
  }) {
    return _then(ActionRequest_BookUnlink(
      card: null == card
          ? _self.card
          : card // ignore: cast_nullable_to_non_nullable
              as String,
      handle: null == handle
          ? _self.handle
          : handle // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_BookExport extends ActionRequest {
  const ActionRequest_BookExport(
      {required this.path, final List<String>? cards})
      : _cards = cards,
        super._();

  final String path;
  final List<String>? _cards;
  List<String>? get cards {
    final value = _cards;
    if (value == null) return null;
    if (_cards is EqualUnmodifiableListView) return _cards;
    // ignore: implicit_dynamic_type
    return EqualUnmodifiableListView(value);
  }

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookExportCopyWith<ActionRequest_BookExport> get copyWith =>
      _$ActionRequest_BookExportCopyWithImpl<ActionRequest_BookExport>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookExport &&
            (identical(other.path, path) || other.path == path) &&
            const DeepCollectionEquality().equals(other._cards, _cards));
  }

  @override
  int get hashCode => Object.hash(
      runtimeType, path, const DeepCollectionEquality().hash(_cards));

  @override
  String toString() {
    return 'ActionRequest.bookExport(path: $path, cards: $cards)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookExportCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookExportCopyWith(ActionRequest_BookExport value,
          $Res Function(ActionRequest_BookExport) _then) =
      _$ActionRequest_BookExportCopyWithImpl;
  @useResult
  $Res call({String path, List<String>? cards});
}

/// @nodoc
class _$ActionRequest_BookExportCopyWithImpl<$Res>
    implements $ActionRequest_BookExportCopyWith<$Res> {
  _$ActionRequest_BookExportCopyWithImpl(this._self, this._then);

  final ActionRequest_BookExport _self;
  final $Res Function(ActionRequest_BookExport) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? path = null,
    Object? cards = freezed,
  }) {
    return _then(ActionRequest_BookExport(
      path: null == path
          ? _self.path
          : path // ignore: cast_nullable_to_non_nullable
              as String,
      cards: freezed == cards
          ? _self._cards
          : cards // ignore: cast_nullable_to_non_nullable
              as List<String>?,
    ));
  }
}

/// @nodoc

class ActionRequest_BookImport extends ActionRequest {
  const ActionRequest_BookImport({required this.path}) : super._();

  final String path;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookImportCopyWith<ActionRequest_BookImport> get copyWith =>
      _$ActionRequest_BookImportCopyWithImpl<ActionRequest_BookImport>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookImport &&
            (identical(other.path, path) || other.path == path));
  }

  @override
  int get hashCode => Object.hash(runtimeType, path);

  @override
  String toString() {
    return 'ActionRequest.bookImport(path: $path)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookImportCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookImportCopyWith(ActionRequest_BookImport value,
          $Res Function(ActionRequest_BookImport) _then) =
      _$ActionRequest_BookImportCopyWithImpl;
  @useResult
  $Res call({String path});
}

/// @nodoc
class _$ActionRequest_BookImportCopyWithImpl<$Res>
    implements $ActionRequest_BookImportCopyWith<$Res> {
  _$ActionRequest_BookImportCopyWithImpl(this._self, this._then);

  final ActionRequest_BookImport _self;
  final $Res Function(ActionRequest_BookImport) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? path = null,
  }) {
    return _then(ActionRequest_BookImport(
      path: null == path
          ? _self.path
          : path // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_BookAccept extends ActionRequest {
  const ActionRequest_BookAccept({required this.suggestion}) : super._();

  final String suggestion;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookAcceptCopyWith<ActionRequest_BookAccept> get copyWith =>
      _$ActionRequest_BookAcceptCopyWithImpl<ActionRequest_BookAccept>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookAccept &&
            (identical(other.suggestion, suggestion) ||
                other.suggestion == suggestion));
  }

  @override
  int get hashCode => Object.hash(runtimeType, suggestion);

  @override
  String toString() {
    return 'ActionRequest.bookAccept(suggestion: $suggestion)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookAcceptCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookAcceptCopyWith(ActionRequest_BookAccept value,
          $Res Function(ActionRequest_BookAccept) _then) =
      _$ActionRequest_BookAcceptCopyWithImpl;
  @useResult
  $Res call({String suggestion});
}

/// @nodoc
class _$ActionRequest_BookAcceptCopyWithImpl<$Res>
    implements $ActionRequest_BookAcceptCopyWith<$Res> {
  _$ActionRequest_BookAcceptCopyWithImpl(this._self, this._then);

  final ActionRequest_BookAccept _self;
  final $Res Function(ActionRequest_BookAccept) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? suggestion = null,
  }) {
    return _then(ActionRequest_BookAccept(
      suggestion: null == suggestion
          ? _self.suggestion
          : suggestion // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_BookDismiss extends ActionRequest {
  const ActionRequest_BookDismiss({required this.suggestion}) : super._();

  final String suggestion;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_BookDismissCopyWith<ActionRequest_BookDismiss> get copyWith =>
      _$ActionRequest_BookDismissCopyWithImpl<ActionRequest_BookDismiss>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_BookDismiss &&
            (identical(other.suggestion, suggestion) ||
                other.suggestion == suggestion));
  }

  @override
  int get hashCode => Object.hash(runtimeType, suggestion);

  @override
  String toString() {
    return 'ActionRequest.bookDismiss(suggestion: $suggestion)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_BookDismissCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_BookDismissCopyWith(ActionRequest_BookDismiss value,
          $Res Function(ActionRequest_BookDismiss) _then) =
      _$ActionRequest_BookDismissCopyWithImpl;
  @useResult
  $Res call({String suggestion});
}

/// @nodoc
class _$ActionRequest_BookDismissCopyWithImpl<$Res>
    implements $ActionRequest_BookDismissCopyWith<$Res> {
  _$ActionRequest_BookDismissCopyWithImpl(this._self, this._then);

  final ActionRequest_BookDismiss _self;
  final $Res Function(ActionRequest_BookDismiss) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? suggestion = null,
  }) {
    return _then(ActionRequest_BookDismiss(
      suggestion: null == suggestion
          ? _self.suggestion
          : suggestion // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_InstallMcp extends ActionRequest {
  const ActionRequest_InstallMcp(
      {required this.client,
      this.scope,
      required this.name,
      this.agent,
      required this.noAgent,
      required this.project,
      this.world,
      required this.preview})
      : super._();

  /// `claude` | `cursor` | `windsurf` | `generic`.
  final String client;

  /// `user` | `project`; `None` takes the client's default.
  final String? scope;
  final String name;
  final String? agent;
  final bool noAgent;
  final String project;

  /// World mount. `None` is the sole-World default.
  final String? world;
  final bool preview;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_InstallMcpCopyWith<ActionRequest_InstallMcp> get copyWith =>
      _$ActionRequest_InstallMcpCopyWithImpl<ActionRequest_InstallMcp>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_InstallMcp &&
            (identical(other.client, client) || other.client == client) &&
            (identical(other.scope, scope) || other.scope == scope) &&
            (identical(other.name, name) || other.name == name) &&
            (identical(other.agent, agent) || other.agent == agent) &&
            (identical(other.noAgent, noAgent) || other.noAgent == noAgent) &&
            (identical(other.project, project) || other.project == project) &&
            (identical(other.world, world) || other.world == world) &&
            (identical(other.preview, preview) || other.preview == preview));
  }

  @override
  int get hashCode => Object.hash(runtimeType, client, scope, name, agent,
      noAgent, project, world, preview);

  @override
  String toString() {
    return 'ActionRequest.installMcp(client: $client, scope: $scope, name: $name, agent: $agent, noAgent: $noAgent, project: $project, world: $world, preview: $preview)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_InstallMcpCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_InstallMcpCopyWith(ActionRequest_InstallMcp value,
          $Res Function(ActionRequest_InstallMcp) _then) =
      _$ActionRequest_InstallMcpCopyWithImpl;
  @useResult
  $Res call(
      {String client,
      String? scope,
      String name,
      String? agent,
      bool noAgent,
      String project,
      String? world,
      bool preview});
}

/// @nodoc
class _$ActionRequest_InstallMcpCopyWithImpl<$Res>
    implements $ActionRequest_InstallMcpCopyWith<$Res> {
  _$ActionRequest_InstallMcpCopyWithImpl(this._self, this._then);

  final ActionRequest_InstallMcp _self;
  final $Res Function(ActionRequest_InstallMcp) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? client = null,
    Object? scope = freezed,
    Object? name = null,
    Object? agent = freezed,
    Object? noAgent = null,
    Object? project = null,
    Object? world = freezed,
    Object? preview = null,
  }) {
    return _then(ActionRequest_InstallMcp(
      client: null == client
          ? _self.client
          : client // ignore: cast_nullable_to_non_nullable
              as String,
      scope: freezed == scope
          ? _self.scope
          : scope // ignore: cast_nullable_to_non_nullable
              as String?,
      name: null == name
          ? _self.name
          : name // ignore: cast_nullable_to_non_nullable
              as String,
      agent: freezed == agent
          ? _self.agent
          : agent // ignore: cast_nullable_to_non_nullable
              as String?,
      noAgent: null == noAgent
          ? _self.noAgent
          : noAgent // ignore: cast_nullable_to_non_nullable
              as bool,
      project: null == project
          ? _self.project
          : project // ignore: cast_nullable_to_non_nullable
              as String,
      world: freezed == world
          ? _self.world
          : world // ignore: cast_nullable_to_non_nullable
              as String?,
      preview: null == preview
          ? _self.preview
          : preview // ignore: cast_nullable_to_non_nullable
              as bool,
    ));
  }
}

/// @nodoc

class ActionRequest_DisplayPairingApprove extends ActionRequest {
  const ActionRequest_DisplayPairingApprove(
      {required this.pairing, required this.label})
      : super._();

  final String pairing;
  final String label;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_DisplayPairingApproveCopyWith<
          ActionRequest_DisplayPairingApprove>
      get copyWith => _$ActionRequest_DisplayPairingApproveCopyWithImpl<
          ActionRequest_DisplayPairingApprove>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_DisplayPairingApprove &&
            (identical(other.pairing, pairing) || other.pairing == pairing) &&
            (identical(other.label, label) || other.label == label));
  }

  @override
  int get hashCode => Object.hash(runtimeType, pairing, label);

  @override
  String toString() {
    return 'ActionRequest.displayPairingApprove(pairing: $pairing, label: $label)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_DisplayPairingApproveCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_DisplayPairingApproveCopyWith(
          ActionRequest_DisplayPairingApprove value,
          $Res Function(ActionRequest_DisplayPairingApprove) _then) =
      _$ActionRequest_DisplayPairingApproveCopyWithImpl;
  @useResult
  $Res call({String pairing, String label});
}

/// @nodoc
class _$ActionRequest_DisplayPairingApproveCopyWithImpl<$Res>
    implements $ActionRequest_DisplayPairingApproveCopyWith<$Res> {
  _$ActionRequest_DisplayPairingApproveCopyWithImpl(this._self, this._then);

  final ActionRequest_DisplayPairingApprove _self;
  final $Res Function(ActionRequest_DisplayPairingApprove) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? pairing = null,
    Object? label = null,
  }) {
    return _then(ActionRequest_DisplayPairingApprove(
      pairing: null == pairing
          ? _self.pairing
          : pairing // ignore: cast_nullable_to_non_nullable
              as String,
      label: null == label
          ? _self.label
          : label // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_DisplayPairingReject extends ActionRequest {
  const ActionRequest_DisplayPairingReject({required this.pairing}) : super._();

  final String pairing;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_DisplayPairingRejectCopyWith<
          ActionRequest_DisplayPairingReject>
      get copyWith => _$ActionRequest_DisplayPairingRejectCopyWithImpl<
          ActionRequest_DisplayPairingReject>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_DisplayPairingReject &&
            (identical(other.pairing, pairing) || other.pairing == pairing));
  }

  @override
  int get hashCode => Object.hash(runtimeType, pairing);

  @override
  String toString() {
    return 'ActionRequest.displayPairingReject(pairing: $pairing)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_DisplayPairingRejectCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_DisplayPairingRejectCopyWith(
          ActionRequest_DisplayPairingReject value,
          $Res Function(ActionRequest_DisplayPairingReject) _then) =
      _$ActionRequest_DisplayPairingRejectCopyWithImpl;
  @useResult
  $Res call({String pairing});
}

/// @nodoc
class _$ActionRequest_DisplayPairingRejectCopyWithImpl<$Res>
    implements $ActionRequest_DisplayPairingRejectCopyWith<$Res> {
  _$ActionRequest_DisplayPairingRejectCopyWithImpl(this._self, this._then);

  final ActionRequest_DisplayPairingReject _self;
  final $Res Function(ActionRequest_DisplayPairingReject) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? pairing = null,
  }) {
    return _then(ActionRequest_DisplayPairingReject(
      pairing: null == pairing
          ? _self.pairing
          : pairing // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_DisplayAssignmentPut extends ActionRequest {
  const ActionRequest_DisplayAssignmentPut(
      {required this.device,
      required this.orbit,
      required this.world,
      required this.surface,
      required this.inputJson,
      required this.theme,
      required this.staleAfterMs,
      required this.onStale,
      this.syncGroup,
      required this.syncMode,
      required this.staticDelayMs,
      this.expiresAtUnixMs})
      : super._();

  final String device;
  final String orbit;
  final String world;
  final String surface;
  final String inputJson;
  final DisplayTheme theme;
  final int staleAfterMs;
  final DisplayStaleAction onStale;
  final String? syncGroup;
  final DisplaySyncMode syncMode;
  final int staticDelayMs;
  final BigInt? expiresAtUnixMs;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_DisplayAssignmentPutCopyWith<
          ActionRequest_DisplayAssignmentPut>
      get copyWith => _$ActionRequest_DisplayAssignmentPutCopyWithImpl<
          ActionRequest_DisplayAssignmentPut>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_DisplayAssignmentPut &&
            (identical(other.device, device) || other.device == device) &&
            (identical(other.orbit, orbit) || other.orbit == orbit) &&
            (identical(other.world, world) || other.world == world) &&
            (identical(other.surface, surface) || other.surface == surface) &&
            (identical(other.inputJson, inputJson) ||
                other.inputJson == inputJson) &&
            (identical(other.theme, theme) || other.theme == theme) &&
            (identical(other.staleAfterMs, staleAfterMs) ||
                other.staleAfterMs == staleAfterMs) &&
            (identical(other.onStale, onStale) || other.onStale == onStale) &&
            (identical(other.syncGroup, syncGroup) ||
                other.syncGroup == syncGroup) &&
            (identical(other.syncMode, syncMode) ||
                other.syncMode == syncMode) &&
            (identical(other.staticDelayMs, staticDelayMs) ||
                other.staticDelayMs == staticDelayMs) &&
            (identical(other.expiresAtUnixMs, expiresAtUnixMs) ||
                other.expiresAtUnixMs == expiresAtUnixMs));
  }

  @override
  int get hashCode => Object.hash(
      runtimeType,
      device,
      orbit,
      world,
      surface,
      inputJson,
      theme,
      staleAfterMs,
      onStale,
      syncGroup,
      syncMode,
      staticDelayMs,
      expiresAtUnixMs);

  @override
  String toString() {
    return 'ActionRequest.displayAssignmentPut(device: $device, orbit: $orbit, world: $world, surface: $surface, inputJson: $inputJson, theme: $theme, staleAfterMs: $staleAfterMs, onStale: $onStale, syncGroup: $syncGroup, syncMode: $syncMode, staticDelayMs: $staticDelayMs, expiresAtUnixMs: $expiresAtUnixMs)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_DisplayAssignmentPutCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_DisplayAssignmentPutCopyWith(
          ActionRequest_DisplayAssignmentPut value,
          $Res Function(ActionRequest_DisplayAssignmentPut) _then) =
      _$ActionRequest_DisplayAssignmentPutCopyWithImpl;
  @useResult
  $Res call(
      {String device,
      String orbit,
      String world,
      String surface,
      String inputJson,
      DisplayTheme theme,
      int staleAfterMs,
      DisplayStaleAction onStale,
      String? syncGroup,
      DisplaySyncMode syncMode,
      int staticDelayMs,
      BigInt? expiresAtUnixMs});
}

/// @nodoc
class _$ActionRequest_DisplayAssignmentPutCopyWithImpl<$Res>
    implements $ActionRequest_DisplayAssignmentPutCopyWith<$Res> {
  _$ActionRequest_DisplayAssignmentPutCopyWithImpl(this._self, this._then);

  final ActionRequest_DisplayAssignmentPut _self;
  final $Res Function(ActionRequest_DisplayAssignmentPut) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? device = null,
    Object? orbit = null,
    Object? world = null,
    Object? surface = null,
    Object? inputJson = null,
    Object? theme = null,
    Object? staleAfterMs = null,
    Object? onStale = null,
    Object? syncGroup = freezed,
    Object? syncMode = null,
    Object? staticDelayMs = null,
    Object? expiresAtUnixMs = freezed,
  }) {
    return _then(ActionRequest_DisplayAssignmentPut(
      device: null == device
          ? _self.device
          : device // ignore: cast_nullable_to_non_nullable
              as String,
      orbit: null == orbit
          ? _self.orbit
          : orbit // ignore: cast_nullable_to_non_nullable
              as String,
      world: null == world
          ? _self.world
          : world // ignore: cast_nullable_to_non_nullable
              as String,
      surface: null == surface
          ? _self.surface
          : surface // ignore: cast_nullable_to_non_nullable
              as String,
      inputJson: null == inputJson
          ? _self.inputJson
          : inputJson // ignore: cast_nullable_to_non_nullable
              as String,
      theme: null == theme
          ? _self.theme
          : theme // ignore: cast_nullable_to_non_nullable
              as DisplayTheme,
      staleAfterMs: null == staleAfterMs
          ? _self.staleAfterMs
          : staleAfterMs // ignore: cast_nullable_to_non_nullable
              as int,
      onStale: null == onStale
          ? _self.onStale
          : onStale // ignore: cast_nullable_to_non_nullable
              as DisplayStaleAction,
      syncGroup: freezed == syncGroup
          ? _self.syncGroup
          : syncGroup // ignore: cast_nullable_to_non_nullable
              as String?,
      syncMode: null == syncMode
          ? _self.syncMode
          : syncMode // ignore: cast_nullable_to_non_nullable
              as DisplaySyncMode,
      staticDelayMs: null == staticDelayMs
          ? _self.staticDelayMs
          : staticDelayMs // ignore: cast_nullable_to_non_nullable
              as int,
      expiresAtUnixMs: freezed == expiresAtUnixMs
          ? _self.expiresAtUnixMs
          : expiresAtUnixMs // ignore: cast_nullable_to_non_nullable
              as BigInt?,
    ));
  }
}

/// @nodoc

class ActionRequest_DisplayAssignmentRevoke extends ActionRequest {
  const ActionRequest_DisplayAssignmentRevoke({required this.assignment})
      : super._();

  final String assignment;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_DisplayAssignmentRevokeCopyWith<
          ActionRequest_DisplayAssignmentRevoke>
      get copyWith => _$ActionRequest_DisplayAssignmentRevokeCopyWithImpl<
          ActionRequest_DisplayAssignmentRevoke>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_DisplayAssignmentRevoke &&
            (identical(other.assignment, assignment) ||
                other.assignment == assignment));
  }

  @override
  int get hashCode => Object.hash(runtimeType, assignment);

  @override
  String toString() {
    return 'ActionRequest.displayAssignmentRevoke(assignment: $assignment)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_DisplayAssignmentRevokeCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_DisplayAssignmentRevokeCopyWith(
          ActionRequest_DisplayAssignmentRevoke value,
          $Res Function(ActionRequest_DisplayAssignmentRevoke) _then) =
      _$ActionRequest_DisplayAssignmentRevokeCopyWithImpl;
  @useResult
  $Res call({String assignment});
}

/// @nodoc
class _$ActionRequest_DisplayAssignmentRevokeCopyWithImpl<$Res>
    implements $ActionRequest_DisplayAssignmentRevokeCopyWith<$Res> {
  _$ActionRequest_DisplayAssignmentRevokeCopyWithImpl(this._self, this._then);

  final ActionRequest_DisplayAssignmentRevoke _self;
  final $Res Function(ActionRequest_DisplayAssignmentRevoke) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? assignment = null,
  }) {
    return _then(ActionRequest_DisplayAssignmentRevoke(
      assignment: null == assignment
          ? _self.assignment
          : assignment // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_DisplayDeviceRevoke extends ActionRequest {
  const ActionRequest_DisplayDeviceRevoke({required this.device}) : super._();

  final String device;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_DisplayDeviceRevokeCopyWith<ActionRequest_DisplayDeviceRevoke>
      get copyWith => _$ActionRequest_DisplayDeviceRevokeCopyWithImpl<
          ActionRequest_DisplayDeviceRevoke>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_DisplayDeviceRevoke &&
            (identical(other.device, device) || other.device == device));
  }

  @override
  int get hashCode => Object.hash(runtimeType, device);

  @override
  String toString() {
    return 'ActionRequest.displayDeviceRevoke(device: $device)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_DisplayDeviceRevokeCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_DisplayDeviceRevokeCopyWith(
          ActionRequest_DisplayDeviceRevoke value,
          $Res Function(ActionRequest_DisplayDeviceRevoke) _then) =
      _$ActionRequest_DisplayDeviceRevokeCopyWithImpl;
  @useResult
  $Res call({String device});
}

/// @nodoc
class _$ActionRequest_DisplayDeviceRevokeCopyWithImpl<$Res>
    implements $ActionRequest_DisplayDeviceRevokeCopyWith<$Res> {
  _$ActionRequest_DisplayDeviceRevokeCopyWithImpl(this._self, this._then);

  final ActionRequest_DisplayDeviceRevoke _self;
  final $Res Function(ActionRequest_DisplayDeviceRevoke) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? device = null,
  }) {
    return _then(ActionRequest_DisplayDeviceRevoke(
      device: null == device
          ? _self.device
          : device // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_DisplayIdentifierAdmitPassphrase extends ActionRequest {
  const ActionRequest_DisplayIdentifierAdmitPassphrase(
      {required this.passphrase})
      : super._();

  final String passphrase;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_DisplayIdentifierAdmitPassphraseCopyWith<
          ActionRequest_DisplayIdentifierAdmitPassphrase>
      get copyWith =>
          _$ActionRequest_DisplayIdentifierAdmitPassphraseCopyWithImpl<
              ActionRequest_DisplayIdentifierAdmitPassphrase>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_DisplayIdentifierAdmitPassphrase &&
            (identical(other.passphrase, passphrase) ||
                other.passphrase == passphrase));
  }

  @override
  int get hashCode => Object.hash(runtimeType, passphrase);

  @override
  String toString() {
    return 'ActionRequest.displayIdentifierAdmitPassphrase(passphrase: $passphrase)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_DisplayIdentifierAdmitPassphraseCopyWith<
    $Res> implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_DisplayIdentifierAdmitPassphraseCopyWith(
          ActionRequest_DisplayIdentifierAdmitPassphrase value,
          $Res Function(ActionRequest_DisplayIdentifierAdmitPassphrase) _then) =
      _$ActionRequest_DisplayIdentifierAdmitPassphraseCopyWithImpl;
  @useResult
  $Res call({String passphrase});
}

/// @nodoc
class _$ActionRequest_DisplayIdentifierAdmitPassphraseCopyWithImpl<$Res>
    implements $ActionRequest_DisplayIdentifierAdmitPassphraseCopyWith<$Res> {
  _$ActionRequest_DisplayIdentifierAdmitPassphraseCopyWithImpl(
      this._self, this._then);

  final ActionRequest_DisplayIdentifierAdmitPassphrase _self;
  final $Res Function(ActionRequest_DisplayIdentifierAdmitPassphrase) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? passphrase = null,
  }) {
    return _then(ActionRequest_DisplayIdentifierAdmitPassphrase(
      passphrase: null == passphrase
          ? _self.passphrase
          : passphrase // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_EnterPresentation extends ActionRequest {
  const ActionRequest_EnterPresentation() : super._();

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_EnterPresentation);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'ActionRequest.enterPresentation()';
  }
}

/// @nodoc

class ActionRequest_PresentHere extends ActionRequest {
  const ActionRequest_PresentHere(
      {required this.orbit,
      required this.world,
      required this.surface,
      required this.input,
      required this.title})
      : super._();

  final String orbit;
  final String world;
  final String surface;
  final String input;
  final String title;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $ActionRequest_PresentHereCopyWith<ActionRequest_PresentHere> get copyWith =>
      _$ActionRequest_PresentHereCopyWithImpl<ActionRequest_PresentHere>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_PresentHere &&
            (identical(other.orbit, orbit) || other.orbit == orbit) &&
            (identical(other.world, world) || other.world == world) &&
            (identical(other.surface, surface) || other.surface == surface) &&
            (identical(other.input, input) || other.input == input) &&
            (identical(other.title, title) || other.title == title));
  }

  @override
  int get hashCode =>
      Object.hash(runtimeType, orbit, world, surface, input, title);

  @override
  String toString() {
    return 'ActionRequest.presentHere(orbit: $orbit, world: $world, surface: $surface, input: $input, title: $title)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_PresentHereCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_PresentHereCopyWith(ActionRequest_PresentHere value,
          $Res Function(ActionRequest_PresentHere) _then) =
      _$ActionRequest_PresentHereCopyWithImpl;
  @useResult
  $Res call(
      {String orbit, String world, String surface, String input, String title});
}

/// @nodoc
class _$ActionRequest_PresentHereCopyWithImpl<$Res>
    implements $ActionRequest_PresentHereCopyWith<$Res> {
  _$ActionRequest_PresentHereCopyWithImpl(this._self, this._then);

  final ActionRequest_PresentHere _self;
  final $Res Function(ActionRequest_PresentHere) _then;

  /// Create a copy of ActionRequest
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? orbit = null,
    Object? world = null,
    Object? surface = null,
    Object? input = null,
    Object? title = null,
  }) {
    return _then(ActionRequest_PresentHere(
      orbit: null == orbit
          ? _self.orbit
          : orbit // ignore: cast_nullable_to_non_nullable
              as String,
      world: null == world
          ? _self.world
          : world // ignore: cast_nullable_to_non_nullable
              as String,
      surface: null == surface
          ? _self.surface
          : surface // ignore: cast_nullable_to_non_nullable
              as String,
      input: null == input
          ? _self.input
          : input // ignore: cast_nullable_to_non_nullable
              as String,
      title: null == title
          ? _self.title
          : title // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class ActionRequest_PresentRefresh extends ActionRequest {
  const ActionRequest_PresentRefresh() : super._();

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_PresentRefresh);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'ActionRequest.presentRefresh()';
  }
}

/// @nodoc

class ActionRequest_LeavePresentation extends ActionRequest {
  const ActionRequest_LeavePresentation() : super._();

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is ActionRequest_LeavePresentation);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'ActionRequest.leavePresentation()';
  }
}

/// @nodoc
mixin _$PresentedScene {
  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType && other is PresentedScene);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'PresentedScene()';
  }
}

/// @nodoc
class $PresentedSceneCopyWith<$Res> {
  $PresentedSceneCopyWith(PresentedScene _, $Res Function(PresentedScene) __);
}

/// Adds pattern-matching-related methods to [PresentedScene].
extension PresentedScenePatterns on PresentedScene {
  /// A variant of `map` that fallback to returning `orElse`.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case _:
  ///     return orElse();
  /// }
  /// ```

  @optionalTypeArgs
  TResult maybeMap<TResult extends Object?>({
    TResult Function(PresentedScene_Frame value)? frame,
    TResult Function(PresentedScene_Blank value)? blank,
    TResult Function(PresentedScene_Unsupported value)? unsupported,
    required TResult orElse(),
  }) {
    final _that = this;
    switch (_that) {
      case PresentedScene_Frame() when frame != null:
        return frame(_that);
      case PresentedScene_Blank() when blank != null:
        return blank(_that);
      case PresentedScene_Unsupported() when unsupported != null:
        return unsupported(_that);
      case _:
        return orElse();
    }
  }

  /// A `switch`-like method, using callbacks.
  ///
  /// Callbacks receives the raw object, upcasted.
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case final Subclass2 value:
  ///     return ...;
  /// }
  /// ```

  @optionalTypeArgs
  TResult map<TResult extends Object?>({
    required TResult Function(PresentedScene_Frame value) frame,
    required TResult Function(PresentedScene_Blank value) blank,
    required TResult Function(PresentedScene_Unsupported value) unsupported,
  }) {
    final _that = this;
    switch (_that) {
      case PresentedScene_Frame():
        return frame(_that);
      case PresentedScene_Blank():
        return blank(_that);
      case PresentedScene_Unsupported():
        return unsupported(_that);
    }
  }

  /// A variant of `map` that fallback to returning `null`.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case _:
  ///     return null;
  /// }
  /// ```

  @optionalTypeArgs
  TResult? mapOrNull<TResult extends Object?>({
    TResult? Function(PresentedScene_Frame value)? frame,
    TResult? Function(PresentedScene_Blank value)? blank,
    TResult? Function(PresentedScene_Unsupported value)? unsupported,
  }) {
    final _that = this;
    switch (_that) {
      case PresentedScene_Frame() when frame != null:
        return frame(_that);
      case PresentedScene_Blank() when blank != null:
        return blank(_that);
      case PresentedScene_Unsupported() when unsupported != null:
        return unsupported(_that);
      case _:
        return null;
    }
  }

  /// A variant of `when` that fallback to an `orElse` callback.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case _:
  ///     return orElse();
  /// }
  /// ```

  @optionalTypeArgs
  TResult maybeWhen<TResult extends Object?>({
    TResult Function(String mediaType, int width, int height, Uint8List bytes)?
        frame,
    TResult Function(String reason)? blank,
    TResult Function(String output)? unsupported,
    required TResult orElse(),
  }) {
    final _that = this;
    switch (_that) {
      case PresentedScene_Frame() when frame != null:
        return frame(_that.mediaType, _that.width, _that.height, _that.bytes);
      case PresentedScene_Blank() when blank != null:
        return blank(_that.reason);
      case PresentedScene_Unsupported() when unsupported != null:
        return unsupported(_that.output);
      case _:
        return orElse();
    }
  }

  /// A `switch`-like method, using callbacks.
  ///
  /// As opposed to `map`, this offers destructuring.
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case Subclass2(:final field2):
  ///     return ...;
  /// }
  /// ```

  @optionalTypeArgs
  TResult when<TResult extends Object?>({
    required TResult Function(
            String mediaType, int width, int height, Uint8List bytes)
        frame,
    required TResult Function(String reason) blank,
    required TResult Function(String output) unsupported,
  }) {
    final _that = this;
    switch (_that) {
      case PresentedScene_Frame():
        return frame(_that.mediaType, _that.width, _that.height, _that.bytes);
      case PresentedScene_Blank():
        return blank(_that.reason);
      case PresentedScene_Unsupported():
        return unsupported(_that.output);
    }
  }

  /// A variant of `when` that fallback to returning `null`
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case _:
  ///     return null;
  /// }
  /// ```

  @optionalTypeArgs
  TResult? whenOrNull<TResult extends Object?>({
    TResult? Function(String mediaType, int width, int height, Uint8List bytes)?
        frame,
    TResult? Function(String reason)? blank,
    TResult? Function(String output)? unsupported,
  }) {
    final _that = this;
    switch (_that) {
      case PresentedScene_Frame() when frame != null:
        return frame(_that.mediaType, _that.width, _that.height, _that.bytes);
      case PresentedScene_Blank() when blank != null:
        return blank(_that.reason);
      case PresentedScene_Unsupported() when unsupported != null:
        return unsupported(_that.output);
      case _:
        return null;
    }
  }
}

/// @nodoc

class PresentedScene_Frame extends PresentedScene {
  const PresentedScene_Frame(
      {required this.mediaType,
      required this.width,
      required this.height,
      required this.bytes})
      : super._();

  /// `png`, `jpeg`, or `webp`.
  final String mediaType;
  final int width;
  final int height;
  final Uint8List bytes;

  /// Create a copy of PresentedScene
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $PresentedScene_FrameCopyWith<PresentedScene_Frame> get copyWith =>
      _$PresentedScene_FrameCopyWithImpl<PresentedScene_Frame>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is PresentedScene_Frame &&
            (identical(other.mediaType, mediaType) ||
                other.mediaType == mediaType) &&
            (identical(other.width, width) || other.width == width) &&
            (identical(other.height, height) || other.height == height) &&
            const DeepCollectionEquality().equals(other.bytes, bytes));
  }

  @override
  int get hashCode => Object.hash(runtimeType, mediaType, width, height,
      const DeepCollectionEquality().hash(bytes));

  @override
  String toString() {
    return 'PresentedScene.frame(mediaType: $mediaType, width: $width, height: $height, bytes: $bytes)';
  }
}

/// @nodoc
abstract mixin class $PresentedScene_FrameCopyWith<$Res>
    implements $PresentedSceneCopyWith<$Res> {
  factory $PresentedScene_FrameCopyWith(PresentedScene_Frame value,
          $Res Function(PresentedScene_Frame) _then) =
      _$PresentedScene_FrameCopyWithImpl;
  @useResult
  $Res call({String mediaType, int width, int height, Uint8List bytes});
}

/// @nodoc
class _$PresentedScene_FrameCopyWithImpl<$Res>
    implements $PresentedScene_FrameCopyWith<$Res> {
  _$PresentedScene_FrameCopyWithImpl(this._self, this._then);

  final PresentedScene_Frame _self;
  final $Res Function(PresentedScene_Frame) _then;

  /// Create a copy of PresentedScene
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? mediaType = null,
    Object? width = null,
    Object? height = null,
    Object? bytes = null,
  }) {
    return _then(PresentedScene_Frame(
      mediaType: null == mediaType
          ? _self.mediaType
          : mediaType // ignore: cast_nullable_to_non_nullable
              as String,
      width: null == width
          ? _self.width
          : width // ignore: cast_nullable_to_non_nullable
              as int,
      height: null == height
          ? _self.height
          : height // ignore: cast_nullable_to_non_nullable
              as int,
      bytes: null == bytes
          ? _self.bytes
          : bytes // ignore: cast_nullable_to_non_nullable
              as Uint8List,
    ));
  }
}

/// @nodoc

class PresentedScene_Blank extends PresentedScene {
  const PresentedScene_Blank({required this.reason}) : super._();

  /// `source_unavailable`, `unsupported`, or `program_ended`.
  final String reason;

  /// Create a copy of PresentedScene
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $PresentedScene_BlankCopyWith<PresentedScene_Blank> get copyWith =>
      _$PresentedScene_BlankCopyWithImpl<PresentedScene_Blank>(
          this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is PresentedScene_Blank &&
            (identical(other.reason, reason) || other.reason == reason));
  }

  @override
  int get hashCode => Object.hash(runtimeType, reason);

  @override
  String toString() {
    return 'PresentedScene.blank(reason: $reason)';
  }
}

/// @nodoc
abstract mixin class $PresentedScene_BlankCopyWith<$Res>
    implements $PresentedSceneCopyWith<$Res> {
  factory $PresentedScene_BlankCopyWith(PresentedScene_Blank value,
          $Res Function(PresentedScene_Blank) _then) =
      _$PresentedScene_BlankCopyWithImpl;
  @useResult
  $Res call({String reason});
}

/// @nodoc
class _$PresentedScene_BlankCopyWithImpl<$Res>
    implements $PresentedScene_BlankCopyWith<$Res> {
  _$PresentedScene_BlankCopyWithImpl(this._self, this._then);

  final PresentedScene_Blank _self;
  final $Res Function(PresentedScene_Blank) _then;

  /// Create a copy of PresentedScene
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? reason = null,
  }) {
    return _then(PresentedScene_Blank(
      reason: null == reason
          ? _self.reason
          : reason // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc

class PresentedScene_Unsupported extends PresentedScene {
  const PresentedScene_Unsupported({required this.output}) : super._();

  final String output;

  /// Create a copy of PresentedScene
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $PresentedScene_UnsupportedCopyWith<PresentedScene_Unsupported>
      get copyWith =>
          _$PresentedScene_UnsupportedCopyWithImpl<PresentedScene_Unsupported>(
              this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is PresentedScene_Unsupported &&
            (identical(other.output, output) || other.output == output));
  }

  @override
  int get hashCode => Object.hash(runtimeType, output);

  @override
  String toString() {
    return 'PresentedScene.unsupported(output: $output)';
  }
}

/// @nodoc
abstract mixin class $PresentedScene_UnsupportedCopyWith<$Res>
    implements $PresentedSceneCopyWith<$Res> {
  factory $PresentedScene_UnsupportedCopyWith(PresentedScene_Unsupported value,
          $Res Function(PresentedScene_Unsupported) _then) =
      _$PresentedScene_UnsupportedCopyWithImpl;
  @useResult
  $Res call({String output});
}

/// @nodoc
class _$PresentedScene_UnsupportedCopyWithImpl<$Res>
    implements $PresentedScene_UnsupportedCopyWith<$Res> {
  _$PresentedScene_UnsupportedCopyWithImpl(this._self, this._then);

  final PresentedScene_Unsupported _self;
  final $Res Function(PresentedScene_Unsupported) _then;

  /// Create a copy of PresentedScene
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? output = null,
  }) {
    return _then(PresentedScene_Unsupported(
      output: null == output
          ? _self.output
          : output // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

/// @nodoc
mixin _$Staleness {
  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType && other is Staleness);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'Staleness()';
  }
}

/// @nodoc
class $StalenessCopyWith<$Res> {
  $StalenessCopyWith(Staleness _, $Res Function(Staleness) __);
}

/// Adds pattern-matching-related methods to [Staleness].
extension StalenessPatterns on Staleness {
  /// A variant of `map` that fallback to returning `orElse`.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case _:
  ///     return orElse();
  /// }
  /// ```

  @optionalTypeArgs
  TResult maybeMap<TResult extends Object?>({
    TResult Function(Staleness_NeverLoaded value)? neverLoaded,
    TResult Function(Staleness_Signalled value)? signalled,
    required TResult orElse(),
  }) {
    final _that = this;
    switch (_that) {
      case Staleness_NeverLoaded() when neverLoaded != null:
        return neverLoaded(_that);
      case Staleness_Signalled() when signalled != null:
        return signalled(_that);
      case _:
        return orElse();
    }
  }

  /// A `switch`-like method, using callbacks.
  ///
  /// Callbacks receives the raw object, upcasted.
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case final Subclass2 value:
  ///     return ...;
  /// }
  /// ```

  @optionalTypeArgs
  TResult map<TResult extends Object?>({
    required TResult Function(Staleness_NeverLoaded value) neverLoaded,
    required TResult Function(Staleness_Signalled value) signalled,
  }) {
    final _that = this;
    switch (_that) {
      case Staleness_NeverLoaded():
        return neverLoaded(_that);
      case Staleness_Signalled():
        return signalled(_that);
    }
  }

  /// A variant of `map` that fallback to returning `null`.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case final Subclass value:
  ///     return ...;
  ///   case _:
  ///     return null;
  /// }
  /// ```

  @optionalTypeArgs
  TResult? mapOrNull<TResult extends Object?>({
    TResult? Function(Staleness_NeverLoaded value)? neverLoaded,
    TResult? Function(Staleness_Signalled value)? signalled,
  }) {
    final _that = this;
    switch (_that) {
      case Staleness_NeverLoaded() when neverLoaded != null:
        return neverLoaded(_that);
      case Staleness_Signalled() when signalled != null:
        return signalled(_that);
      case _:
        return null;
    }
  }

  /// A variant of `when` that fallback to an `orElse` callback.
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case _:
  ///     return orElse();
  /// }
  /// ```

  @optionalTypeArgs
  TResult maybeWhen<TResult extends Object?>({
    TResult Function()? neverLoaded,
    TResult Function(String field0)? signalled,
    required TResult orElse(),
  }) {
    final _that = this;
    switch (_that) {
      case Staleness_NeverLoaded() when neverLoaded != null:
        return neverLoaded();
      case Staleness_Signalled() when signalled != null:
        return signalled(_that.field0);
      case _:
        return orElse();
    }
  }

  /// A `switch`-like method, using callbacks.
  ///
  /// As opposed to `map`, this offers destructuring.
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case Subclass2(:final field2):
  ///     return ...;
  /// }
  /// ```

  @optionalTypeArgs
  TResult when<TResult extends Object?>({
    required TResult Function() neverLoaded,
    required TResult Function(String field0) signalled,
  }) {
    final _that = this;
    switch (_that) {
      case Staleness_NeverLoaded():
        return neverLoaded();
      case Staleness_Signalled():
        return signalled(_that.field0);
    }
  }

  /// A variant of `when` that fallback to returning `null`
  ///
  /// It is equivalent to doing:
  /// ```dart
  /// switch (sealedClass) {
  ///   case Subclass(:final field):
  ///     return ...;
  ///   case _:
  ///     return null;
  /// }
  /// ```

  @optionalTypeArgs
  TResult? whenOrNull<TResult extends Object?>({
    TResult? Function()? neverLoaded,
    TResult? Function(String field0)? signalled,
  }) {
    final _that = this;
    switch (_that) {
      case Staleness_NeverLoaded() when neverLoaded != null:
        return neverLoaded();
      case Staleness_Signalled() when signalled != null:
        return signalled(_that.field0);
      case _:
        return null;
    }
  }
}

/// @nodoc

class Staleness_NeverLoaded extends Staleness {
  const Staleness_NeverLoaded() : super._();

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType && other is Staleness_NeverLoaded);
  }

  @override
  int get hashCode => runtimeType.hashCode;

  @override
  String toString() {
    return 'Staleness.neverLoaded()';
  }
}

/// @nodoc

class Staleness_Signalled extends Staleness {
  const Staleness_Signalled(this.field0) : super._();

  final String field0;

  /// Create a copy of Staleness
  /// with the given fields replaced by the non-null parameter values.
  @JsonKey(includeFromJson: false, includeToJson: false)
  @pragma('vm:prefer-inline')
  $Staleness_SignalledCopyWith<Staleness_Signalled> get copyWith =>
      _$Staleness_SignalledCopyWithImpl<Staleness_Signalled>(this, _$identity);

  @override
  bool operator ==(Object other) {
    return identical(this, other) ||
        (other.runtimeType == runtimeType &&
            other is Staleness_Signalled &&
            (identical(other.field0, field0) || other.field0 == field0));
  }

  @override
  int get hashCode => Object.hash(runtimeType, field0);

  @override
  String toString() {
    return 'Staleness.signalled(field0: $field0)';
  }
}

/// @nodoc
abstract mixin class $Staleness_SignalledCopyWith<$Res>
    implements $StalenessCopyWith<$Res> {
  factory $Staleness_SignalledCopyWith(
          Staleness_Signalled value, $Res Function(Staleness_Signalled) _then) =
      _$Staleness_SignalledCopyWithImpl;
  @useResult
  $Res call({String field0});
}

/// @nodoc
class _$Staleness_SignalledCopyWithImpl<$Res>
    implements $Staleness_SignalledCopyWith<$Res> {
  _$Staleness_SignalledCopyWithImpl(this._self, this._then);

  final Staleness_Signalled _self;
  final $Res Function(Staleness_Signalled) _then;

  /// Create a copy of Staleness
  /// with the given fields replaced by the non-null parameter values.
  @pragma('vm:prefer-inline')
  $Res call({
    Object? field0 = null,
  }) {
    return _then(Staleness_Signalled(
      null == field0
          ? _self.field0
          : field0 // ignore: cast_nullable_to_non_nullable
              as String,
    ));
  }
}

// dart format on
