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
    TResult Function(String orbit, String entryPath)? open,
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
    required TResult orElse(),
  }) {
    final _that = this;
    switch (_that) {
      case ActionRequest_Refresh() when refresh != null:
        return refresh();
      case ActionRequest_Open() when open != null:
        return open(_that.orbit, _that.entryPath);
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
    required TResult Function(String orbit, String entryPath) open,
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
  }) {
    final _that = this;
    switch (_that) {
      case ActionRequest_Refresh():
        return refresh();
      case ActionRequest_Open():
        return open(_that.orbit, _that.entryPath);
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
    TResult? Function(String orbit, String entryPath)? open,
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
  }) {
    final _that = this;
    switch (_that) {
      case ActionRequest_Refresh() when refresh != null:
        return refresh();
      case ActionRequest_Open() when open != null:
        return open(_that.orbit, _that.entryPath);
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
  const ActionRequest_Open({required this.orbit, required this.entryPath})
      : super._();

  final String orbit;
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
            (identical(other.orbit, orbit) || other.orbit == orbit) &&
            (identical(other.entryPath, entryPath) ||
                other.entryPath == entryPath));
  }

  @override
  int get hashCode => Object.hash(runtimeType, orbit, entryPath);

  @override
  String toString() {
    return 'ActionRequest.open(orbit: $orbit, entryPath: $entryPath)';
  }
}

/// @nodoc
abstract mixin class $ActionRequest_OpenCopyWith<$Res>
    implements $ActionRequestCopyWith<$Res> {
  factory $ActionRequest_OpenCopyWith(
          ActionRequest_Open value, $Res Function(ActionRequest_Open) _then) =
      _$ActionRequest_OpenCopyWithImpl;
  @useResult
  $Res call({String orbit, String entryPath});
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
    Object? orbit = null,
    Object? entryPath = null,
  }) {
    return _then(ActionRequest_Open(
      orbit: null == orbit
          ? _self.orbit
          : orbit // ignore: cast_nullable_to_non_nullable
              as String,
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
