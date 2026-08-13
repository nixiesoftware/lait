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
/// not define. That is a stronger guarantee than a convention, and it is
/// checkable by grep — which is what the lint in `covalence_lints` does.
library;

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import '../bridge/api.dart' as api;
import '../bridge/frb_generated.dart';

export '../bridge/api.dart'
    show
        ActionRequest,
        ActionRequest_Open,
        ActionRequest_Refresh,
        ActionRequest_RestartDevice,
        ActionRequest_StartDevice,
        ActionRequest_StopDevice,
        ClientView,
        DeviceRow,
        FailureRow,
        HeadRow,
        HostFacts,
        LibraryRow,
        NoticeRow,
        PlacementView,
        Staleness,
        Staleness_NeverLoaded,
        Staleness_Signalled,
        Unopenable;

/// The core, and the last view it produced.
///
/// A [ValueListenable] rather than a stream the surfaces subscribe to
/// individually: every surface draws the same moment, and a widget that had its
/// own subscription could paint a frame out of step with its siblings. One
/// listenable is one moment.
class Client {
  Client._(this._view);

  final ValueNotifier<api.ClientView> _view;

  ValueListenable<api.ClientView> get view => _view;

  /// Bring the core up and start listening.
  ///
  /// Neither path is computed here. `stateRoot` and `sidecar` are resolved by
  /// the core against the running executable, because a path worked out on this
  /// side would be a second opinion about where the installation keeps its
  /// things — and the two would differ on exactly the machine where it
  /// mattered.
  static Future<Client> start() async {
    await Core.init();
    api.start(stateRoot: null, sidecar: null);
    final client = Client._(ValueNotifier(api.current()));
    api.watch().listen((next) => client._view.value = next);
    // The first read has to be asked for. Nothing in the core polls on its own
    // schedule before somebody is looking.
    api.dispatch(action: const api.ActionRequest.refresh());
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
}
