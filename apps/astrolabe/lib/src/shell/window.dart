/// The one window contract for every Astrolabe-owned desktop surface.
///
/// New windows get both their native configuration and their visible frame
/// here. Keeping those halves together prevents a secondary surface from
/// accidentally falling back to an operating-system title bar that disagrees
/// with the active Astrolabe theme.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/widgets.dart';
import 'package:window_manager/window_manager.dart';

import 'caption.dart';
import 'type.dart';

/// The title band's Windows-sized height. It is chrome, not page spacing.
const double kBarHeight = 48;

enum AstrolabeWindowClosePolicy {
  /// Keep the owning process and its active Worlds available from the tray.
  hide,

  /// Close a disposable secondary process, leaving the main client untouched.
  close,
}

typedef AstrolabeCaptionBuilder = Widget Function(
  BuildContext context,
  BoxConstraints windowConstraints,
);

/// Produces the native half of an Astrolabe window.
///
/// Callers supply only geometry and identity. The hidden system title bar is
/// invariant so every process must pair these options with
/// [AstrolabeWindowFrame].
WindowOptions astrolabeWindowOptions({
  required Size size,
  required Size minimumSize,
  required String title,
  bool center = true,
}) {
  return WindowOptions(
    size: size,
    minimumSize: minimumSize,
    center: center,
    title: title,
    titleBarStyle: TitleBarStyle.hidden,
  );
}

/// Shows a configured Astrolabe window without repeating platform setup at
/// each process entrypoint.
Future<void> showAstrolabeWindow(WindowOptions options) {
  return windowManager.waitUntilReadyToShow(
    options,
    () async {
      await windowManager.show();
      await windowManager.focus();
    },
  );
}

/// Shared dark/light-aware chrome for both the primary client and secondary
/// windows. The caption owns movement, maximise state, and system-sized window
/// controls; callers own only the content placed between brand and controls.
class AstrolabeWindowFrame extends StatefulWidget {
  const AstrolabeWindowFrame({
    super.key,
    required this.body,
    required this.closePolicy,
    this.title,
    this.captionBuilder,
    this.wordmarkMinWidth,
  }) : assert(title != null || captionBuilder != null);

  final Widget body;
  final AstrolabeWindowClosePolicy closePolicy;
  final String? title;
  final AstrolabeCaptionBuilder? captionBuilder;

  /// When null, the wordmark is always shown. Primary chrome may hide it at a
  /// narrow breakpoint to preserve its navigation targets.
  final double? wordmarkMinWidth;

  @override
  State<AstrolabeWindowFrame> createState() => _AstrolabeWindowFrameState();
}

class _AstrolabeWindowFrameState extends State<AstrolabeWindowFrame>
    with WindowListener {
  bool _maximised = false;

  @override
  void initState() {
    super.initState();
    windowManager.addListener(this);
  }

  @override
  void dispose() {
    windowManager.removeListener(this);
    super.dispose();
  }

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

  Future<void> _close() => switch (widget.closePolicy) {
        AstrolabeWindowClosePolicy.hide => windowManager.hide(),
        AstrolabeWindowClosePolicy.close => windowManager.close(),
      };

  @override
  Widget build(BuildContext context) {
    return ColoredBox(
      color: context.surface.l50,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _Caption(
            title: widget.title,
            builder: widget.captionBuilder,
            wordmarkMinWidth: widget.wordmarkMinWidth,
            maximised: _maximised,
            onToggleMaximise: _toggleMaximise,
            closePolicy: widget.closePolicy,
            onClose: _close,
          ),
          Expanded(child: widget.body),
        ],
      ),
    );
  }
}

class _Caption extends StatelessWidget {
  const _Caption({
    required this.title,
    required this.builder,
    required this.wordmarkMinWidth,
    required this.maximised,
    required this.onToggleMaximise,
    required this.closePolicy,
    required this.onClose,
  });

  final String? title;
  final AstrolabeCaptionBuilder? builder;
  final double? wordmarkMinWidth;
  final bool maximised;
  final Future<void> Function() onToggleMaximise;
  final AstrolabeWindowClosePolicy closePolicy;
  final Future<void> Function() onClose;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return t.box.height(
      // reason: the caption follows the operating system's target size rather
      // than the content rhythm used below it.
      TokenEscape.rawSize(kBarHeight),
      child: Container(
        decoration: BoxDecoration(
          color: context.surface.l100,
          border: t.stroke.edge(bottom: context.border.l500),
        ),
        child: GestureDetector(
          behavior: HitTestBehavior.translucent,
          onPanStart: (_) => windowManager.startDragging(),
          onDoubleTap: onToggleMaximise,
          child: LayoutBuilder(
            builder: (context, constraints) {
              final showWordmark = wordmarkMinWidth == null ||
                  constraints.maxWidth >= wordmarkMinWidth!;
              return Row(
                children: [
                  t.gap.x(Space.xl3),
                  if (showWordmark) ...[
                    Text(
                      'ASTROLABE',
                      style: context.factLabelStyle.copyWith(
                        color: context.text.l950,
                        fontWeight: FontWeight.w700,
                        letterSpacing: 1.4,
                      ),
                    ),
                    t.gap.x(builder == null ? Space.xl3 : Space.xl5),
                  ],
                  if (builder != null)
                    Expanded(child: builder!(context, constraints))
                  else ...[
                    Container(
                      width: t.stroke.xxs,
                      height: 16,
                      color: context.border.l500,
                    ),
                    t.gap.x(Space.xl3),
                    Expanded(
                      child: Text(
                        title!,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: context.bodyStyle,
                      ),
                    ),
                  ],
                  CaptionControls(
                    height: kBarHeight,
                    maximised: maximised,
                    onMinimise: windowManager.minimize,
                    onToggleMaximise: onToggleMaximise,
                    onClose: onClose,
                    closeTooltip: closePolicy == AstrolabeWindowClosePolicy.hide
                        ? 'Close (it keeps serving in the tray)'
                        : 'Close window',
                  ),
                ],
              );
            },
          ),
        ),
      ),
    );
  }
}
