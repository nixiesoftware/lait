/// The only file in this application that touches the bridge.
///
/// Revision 7 of the Plan reintroduced a boundary between the model and the
/// interface, and named five rules that keep it safe. Two of them are
/// structural, and this file is where the structure lives:
///
/// * the Rust core owns `App`, and Dart receives whole immutable projections;
/// * every mutation crosses as an `ActionRequest`, and there is no other route.
///
/// Both hold because nothing else imports `../bridge/`. A surface reaches state
/// through [ClientScope] and asks for things through [Client.dispatch]; it has
/// no way to construct a view, mutate one, or invent a request the core does
/// not define.
///
/// **That rule is held by review, not by a machine.** `covalence_lints` carries
/// eight rules and every one of them is about the token closure — none of them
/// knows what this bridge is. Until one does, the check is:
///
/// ```sh
/// grep -rn "import.*bridge/" lib/ --include=*.dart | grep -v "^lib/src/bridge/"
/// ```
///
/// which should answer with this file and nothing else.
library;

import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
// `_for_generated` rather than the main barrel because that is where
// `ExternalLibrary` lives — and it is the type `Core.init` already takes, so
// naming it here is reaching for the signature's own vocabulary rather than
// into an internal.
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart'
    show ExternalLibrary;

import '../bridge/api.dart' as api;
import '../bridge/frb_generated.dart';

export '../bridge/api.dart'
    show
        ActionRequest,
        ClientView,
        DeviceRow,
        DiagnosisRow,
        FailureRow,
        GateRow,
        GateState,
        HeadRow,
        HostFacts,
        LibraryRow,
        MemberRow,
        Missing,
        NoticeRow,
        OrbitRow,
        PlacementView,
        RouteRow,
        SpaceRow,
        Staleness,
        Staleness_NeverLoaded,
        Staleness_Signalled,
        StorageRow,
        Unopenable,
        BookFacts,
        SuggestionRow,
        CardRow;

/// The core library, resolved for the process that is about to load it.
///
/// Passed explicitly so the generated loader's own hint is never consulted. That
/// hint is wrong twice over: it names `tools/astrolabe/target/`, a directory a
/// cargo **workspace** never creates, and it hardcodes `release`. The first is
/// merely dead; the second is the dangerous half, because it is the same
/// mistake `windows/CMakeLists.txt` documents fixing — a debug interface over a
/// release core, which is a configuration nobody runs on purpose. The bridge's
/// config offers no way to set it, so the decision is made here instead.
///
/// Three candidates, first hit wins. Returning `null` means "no opinion", and
/// the bridge falls back to its own search — which finds the core beside the
/// executable on every platform this ships to.
ExternalLibrary? _core() {
  final name = switch (Platform.operatingSystem) {
    'windows' => 'astrolabe.dll',
    'macos' => 'libastrolabe.dylib',
    _ => 'libastrolabe.so',
  };

  // 1. The bridge's own override. Honoured first so a harness can aim this at
  //    any build it likes without touching the app.
  final override =
      Platform.environment['FRB_DART_LOAD_EXTERNAL_LIBRARY_NATIVE_LIB_DIR'];
  if (override != null && override.isNotEmpty) {
    final at = '$override/$name';
    if (File(at).existsSync()) return ExternalLibrary.open(at);
  }

  // 2. Beside the running executable — a packaged install, and `flutter run`.
  //    The only candidate that still holds once this ships.
  final beside = '${File(Platform.resolvedExecutable).parent.path}/$name';
  if (File(beside).existsSync()) return ExternalLibrary.open(beside);

  // 3. The workspace's target directory, for THIS build's profile. The case a
  //    test process is in: its executable is the Dart VM, somewhere else
  //    entirely, so there is no core beside it. `kDebugMode` rather than a
  //    literal, which is the whole point of replacing the generated hint.
  final profile = kDebugMode ? 'debug' : 'release';
  var dir = Directory.current.absolute;
  for (var up = 0; up < 6; up++) {
    final at = '${dir.path}/target/$profile/$name';
    if (File(at).existsSync()) return ExternalLibrary.open(at);
    final parent = dir.parent;
    if (parent.path == dir.path) break;
    dir = parent;
  }

  return null;
}

/// The core, and the last view it produced.
///
/// A [ValueListenable] rather than a stream the surfaces subscribe to
/// individually: every surface draws the same moment, and a widget that had its
/// own subscription could paint a frame out of step with its siblings. One
/// listenable is one moment.
class Client {
  Client._(this._view, [this._onDispatch]);

  /// A client over a canned view, for tests.
  ///
  /// The generated view classes are ordinary Dart objects, so a surface can be
  /// driven with no bridge, no core, no daemon and no window — which is the
  /// property the interaction tests on the retiring interface had, and the one
  /// worth keeping. `onDispatch` records what a control asked for, which is the
  /// other half: press a real control, read what it asked for.
  @visibleForTesting
  factory Client.canned(
    api.ClientView view, {
    void Function(api.ActionRequest) onDispatch = _ignore,
  }) =>
      Client._(ValueNotifier(view), onDispatch);

  static void _ignore(api.ActionRequest _) {}

  final ValueNotifier<api.ClientView> _view;

  /// Set only by [Client.canned]. In production the bridge is the sink.
  final void Function(api.ActionRequest)? _onDispatch;

  ValueListenable<api.ClientView> get view => _view;

  /// Bring the core up and start listening.
  ///
  /// Neither path is computed here. `stateRoot` and `sidecar` are resolved by
  /// the core against the running executable, because a path worked out on this
  /// side would be a second opinion about where the installation keeps its
  /// things — and the two would differ on exactly the machine where it
  /// mattered.
  static Future<Client> start() async {
    await Core.init(externalLibrary: _core());
    api.start(stateRoot: null, sidecar: null);
    final client = Client._(ValueNotifier(api.current()));
    api.watch().listen((next) => client._view.value = next);
    // The core establishes its signal stream and performs the first read. A
    // second refresh here would give startup two owners and turn one genuine
    // startup failure into two records.
    return client;
  }

  /// Ask for something, and take the view as it stands the instant it was
  /// asked for.
  ///
  /// Applying the returned view immediately is what makes "a control is
  /// disabled the moment it is clicked" true across a boundary. Waiting for the
  /// stream would leave the control live for one round trip, which is long
  /// enough to press it four times and read three refusals.
  void dispatch(api.ActionRequest action) {
    final record = _onDispatch;
    if (record != null) {
      record(action);
      return;
    }
    _view.value = api.dispatch(action: action);
  }

  /// Whether an action is already under way.
  ///
  /// The key vocabulary is the core's; this only asks whether a key it sent is
  /// in the set it sent. A surface that built its own key would be guessing at
  /// the core's naming, and would silently stop matching the day it changed.
  bool isInFlight(String key) => _view.value.inFlight.contains(key);
}

/// Puts the client where every surface can reach it and nowhere else.
class ClientScope extends InheritedNotifier<ValueListenable<api.ClientView>> {
  ClientScope({super.key, required this.client, required super.child})
      : super(notifier: client.view);

  final Client client;

  static Client of(BuildContext context) {
    final scope = context.dependOnInheritedWidgetOfExactType<ClientScope>();
    assert(scope != null, 'No ClientScope above this widget.');
    return scope!.client;
  }

  /// The current view, and a subscription to the next one.
  static api.ClientView watch(BuildContext context) => of(context).view.value;

  @override
  bool updateShouldNotify(ClientScope oldWidget) =>
      client != oldWidget.client || super.updateShouldNotify(oldWidget);
}

/// The action keys the core uses, mirrored for the one thing a surface needs
/// them for: asking whether its own action is in flight.
///
/// Kept beside the gateway rather than spread through the surfaces, so that a
/// change to the core's key format is one edit here and a failing test, rather
/// than seven controls that quietly stop disabling themselves.
abstract final class ActionKeys {
  static const String refresh = 'refresh';
  static String open(String orbit, String entryPath) => 'open:$orbit$entryPath';
  static String startDevice(String id) => 'device.start:$id';
  static String stopDevice(String id) => 'device.stop:$id';
  static String restartDevice(String id) => 'device.restart:$id';
  static String forceStopDevice(String id) => 'device.force-stop:$id';
  static const String stopAllOwned = 'device.stop-all';
  static String readSpace(String orbit) => 'space.read:$orbit';
  static const String startHead = 'head.start';
  static String stopHead(String id) => 'head.stop:$id';
  static String forgetOrbit(String space) => 'orbit.forget:$space';
  static const String bookPutNew = 'book.put';
  static String bookPut(String card) => 'book.put:$card';
  static String bookDelete(String card) => 'book.delete:$card';
  static String bookMerge(String from, String into) => 'book.merge:$from:$into';
  static String bookClaim(String card) => 'book.claim:$card';
  static String bookLink(String card) => 'book.link:$card';
  static String bookUnlink(String card) => 'book.unlink:$card';
  static String bookAccept(String suggestion) => 'book.accept:$suggestion';
  static String bookDismiss(String suggestion) => 'book.dismiss:$suggestion';
  static const String bookExport = 'book.export';
  static const String bookImport = 'book.import';
}
