/// The window: one bar across the top, and a page under it.
///
/// The bar is the title bar. It carries primary navigation as pills on the
/// left, the one action that belongs on chrome rather than on a page in the
/// middle-right, and the window's own three controls flush with the corner.
/// Its contents are inset to the page margin so the first nav item lines up
/// with the first word of every surface below it; the bar itself is full bleed,
/// because a header inset from the window edge reads as a row of controls that
/// happen to be at the top rather than as chrome.
library;

import 'dart:async';

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:window_manager/window_manager.dart';

import '../core/client.dart';
import '../surfaces/surfaces.dart';
import 'caption.dart';
import 'record.dart';

/// The bar's height. Named for what it holds rather than taken from the
/// control ladder: a bar is sized by its contents, and this one carries seven
/// names, a control and the window's own three.
const double kBarHeight = 44;

class AstrolabeShell extends StatefulWidget {
  const AstrolabeShell({super.key});

  @override
  State<AstrolabeShell> createState() => _AstrolabeShellState();
}

class _AstrolabeShellState extends State<AstrolabeShell> with WindowListener {
  Surface _surface = Surface.library;
  bool _maximised = false;

  @override
  void initState() {
    super.initState();
    windowManager.addListener(this);
    unawaited(_readMaximised());
  }

  @override
  void dispose() {
    windowManager.removeListener(this);
    super.dispose();
  }

  Future<void> _readMaximised() async {
    final maximised = await windowManager.isMaximized();
    if (mounted) setState(() => _maximised = maximised);
  }

  // Asked of the platform rather than remembered: a window can be maximised by
  // routes this client never sees — `Win`+`↑`, a snap gesture, a double-click
  // on the bar — and a remembered flag draws the wrong mark the first time one
  // of those happens, then does the wrong thing once.
  @override
  void onWindowMaximize() => setState(() => _maximised = true);

  @override
  void onWindowUnmaximize() => setState(() => _maximised = false);

  Future<void> _toggleMaximise() async {
    if (await windowManager.isMaximized()) {
      await windowManager.unmaximize();
    } else {
      await windowManager.maximize();
    }
  }

  @override
  Widget build(BuildContext context) {
    return Shortcuts(
      shortcuts: <ShortcutActivator, Intent>{
        for (final (index, surface) in Surface.values.indexed)
          SingleActivator(_digits[index], control: true): _ShowSurface(surface),
        const SingleActivator(LogicalKeyboardKey.f5): const _Reread(),
      },
      child: Actions(
        actions: <Type, Action<Intent>>{
          _ShowSurface: CallbackAction<_ShowSurface>(
            onInvoke: (intent) => setState(() => _surface = intent.surface),
          ),
          _Reread: CallbackAction<_Reread>(
            onInvoke: (_) => ClientScope.of(context)
                .dispatch(const ActionRequest.refresh()),
          ),
        },
        child: Focus(
          autofocus: true,
          child: ColoredBox(
            color: context.surface.l50,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _Bar(
                  surface: _surface,
                  maximised: _maximised,
                  onSurface: (surface) => setState(() => _surface = surface),
                  onToggleMaximise: _toggleMaximise,
                ),
                Expanded(child: SurfacePage(surface: _surface)),
                // What happened sits under the surface and never scrolls away
                // with it: a refusal that fell off the bottom of a long page is
                // a refusal nobody read.
                const Padding(
                  padding: EdgeInsets.fromLTRB(16, 0, 16, 12),
                  child: RecordStrip(),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

const List<LogicalKeyboardKey> _digits = [
  LogicalKeyboardKey.digit1,
  LogicalKeyboardKey.digit2,
  LogicalKeyboardKey.digit3,
  LogicalKeyboardKey.digit4,
  LogicalKeyboardKey.digit5,
  LogicalKeyboardKey.digit6,
  LogicalKeyboardKey.digit7,
];

class _ShowSurface extends Intent {
  const _ShowSurface(this.surface);
  final Surface surface;
}

class _Reread extends Intent {
  const _Reread();
}

class _Bar extends StatelessWidget {
  const _Bar({
    required this.surface,
    required this.maximised,
    required this.onSurface,
    required this.onToggleMaximise,
  });

  final Surface surface;
  final bool maximised;
  final ValueChanged<Surface> onSurface;
  final Future<void> Function() onToggleMaximise;

  @override
  Widget build(BuildContext context) {
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final rereading = view.inFlight.contains(ActionKeys.refresh);

    return SizedBox(
      height: kBarHeight,
      child: ColoredBox(
        color: context.surface.l100,
        // The whole bar drags the window, and the controls on it are laid over
        // that gesture. A drag is what moves the window rather than a press:
        // handing the platform its modal move loop the instant a button went
        // down would eat the second half of every double-click.
        child: GestureDetector(
          behavior: HitTestBehavior.translucent,
          onPanStart: (_) => windowManager.startDragging(),
          onDoubleTap: onToggleMaximise,
          child: Row(
            children: [
              const SizedBox(width: 16),
              for (final candidate in Surface.values) ...[
                Button(
                  onPressed: () => onSurface(candidate),
                  label: candidate.title,
                  // The selected state travels into the semantic tree, which is
                  // what a screen reader reads out. A pill that only looked
                  // selected would announce nothing.
                  active: candidate == surface,
                  variant: ButtonVariant.ghost,
                  size: ButtonSize.sm,
                ),
                const SizedBox(width: 4),
              ],
              const Spacer(),
              Button(
                onPressed: rereading
                    ? null
                    : () => client.dispatch(const ActionRequest.refresh()),
                label: 'Refresh',
                // Live during its own action would let a person queue six
                // re-reads by clicking a control that looked idle.
                isLoading: rereading,
                variant: ButtonVariant.ghost,
                size: ButtonSize.sm,
                tooltip: 'Read this machine again (F5)',
              ),
              const SizedBox(width: 8),
              CaptionControls(
                height: kBarHeight,
                maximised: maximised,
                onMinimise: windowManager.minimize,
                onToggleMaximise: onToggleMaximise,
                // Closing minimises to the tray: a person who clicked the wrong
                // X did not ask their Spaces to stop converging, and the daemon
                // outlives every window by design.
                onClose: () async => windowManager.hide(),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
