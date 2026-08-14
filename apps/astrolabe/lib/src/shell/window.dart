/// The one window contract for every Astrolabe-owned desktop surface.
///
/// New windows get both their native configuration and their visible frame
/// here. Keeping those halves together prevents a secondary surface from
/// accidentally falling back to an operating-system title bar that disagrees
/// with the active Astrolabe theme.
library;

import 'dart:async';

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:window_manager/window_manager.dart';

import 'caption.dart';
import 'type.dart';

/// Native configuration for an owned top-level window.
///
/// Geometry remains a host concern: Covalence describes the visible role, and
/// this value describes the HWND that carries it.
@immutable
class OwnedWindowConfiguration {
  const OwnedWindowConfiguration({
    required this.key,
    required this.title,
    required this.size,
    required this.minimumSize,
    required this.dark,
  });

  final String key;
  final String title;
  final Size size;
  final Size minimumSize;
  final bool dark;

  Map<String, Object> toMap() => {
        'key': key,
        'title': title,
        'width': size.width,
        'height': size.height,
        'minimumWidth': minimumSize.width,
        'minimumHeight': minimumSize.height,
        'dark': dark,
      };
}

/// How a window is configured, moved, sized, and closed.
///
/// The main engine uses [window_manager]. A sub-engine must not: that
/// plugin is registered only on the main engine, and a second copy would
/// fight it for the process. [NativeWindowControlHost] talks to the runner
/// instead — the alternative CLIENT-43 named for the book window.
abstract class WindowControlHost {
  Future<void> configureOwned(OwnedWindowConfiguration configuration);
  Future<void> minimize();
  Future<void> toggleMaximize();
  Future<bool> isMaximized();
  Future<void> startDragging();
  Future<void> hide();
  Future<void> close();
}

/// The main engine, and the World-settings process.
class ManagerWindowControlHost implements WindowControlHost {
  const ManagerWindowControlHost();

  @override
  Future<void> configureOwned(OwnedWindowConfiguration configuration) async {}

  @override
  Future<void> minimize() => windowManager.minimize();

  @override
  Future<void> toggleMaximize() async {
    if (await windowManager.isMaximized()) {
      await windowManager.unmaximize();
    } else {
      await windowManager.maximize();
    }
  }

  @override
  Future<bool> isMaximized() => windowManager.isMaximized();

  @override
  Future<void> startDragging() => windowManager.startDragging();

  @override
  Future<void> hide() => windowManager.hide();

  @override
  Future<void> close() => windowManager.close();
}

/// A sub-engine's chrome. The runner owns the HWND; this channel is
/// registered only there.
class NativeWindowControlHost implements WindowControlHost {
  const NativeWindowControlHost();

  static const MethodChannel _channel =
      MethodChannel('astrolabe/window_chrome');

  @override
  Future<void> configureOwned(OwnedWindowConfiguration configuration) async {
    // The engine and its channel are installed by the same native callback.
    // A first-frame call normally lands after registration; the short retry
    // also covers a scheduler interleave without ever revealing an
    // unconfigured native window.
    for (var attempt = 0; attempt < 4; attempt += 1) {
      try {
        await _channel.invokeMethod<void>(
          'configure_owned',
          configuration.toMap(),
        );
        return;
      } on MissingPluginException {
        if (attempt == 3) return;
        await Future<void>.delayed(const Duration(milliseconds: 16));
      }
    }
  }

  @override
  Future<void> minimize() => _channel.invokeMethod<void>('minimize');

  @override
  Future<void> toggleMaximize() =>
      _channel.invokeMethod<void>('toggle_maximize');

  @override
  Future<bool> isMaximized() async =>
      await _channel.invokeMethod<bool>('is_maximized') ?? false;

  @override
  Future<void> startDragging() => _channel.invokeMethod<void>('start_drag');

  @override
  Future<void> hide() => _channel.invokeMethod<void>('hide');

  @override
  Future<void> close() => _channel.invokeMethod<void>('close');

  /// The OS window text — what the taskbar and Alt-Tab call this window.
  /// Not on [WindowControlHost]: the main window's title is set at startup by
  /// `window_manager`, and only a sub-engine needs a way to name itself.
  Future<void> setTitle(String title) =>
      _channel.invokeMethod<void>('set_title', title);
}

/// The title band's Windows-sized height. It is chrome, not page spacing.
const double kBarHeight = 48;

enum AstrolabeWindowClosePolicy {
  /// Keep the owning process and its active Worlds available from the tray.
  hide,

  /// Close a disposable owned window, leaving the main client untouched.
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
  const AstrolabeWindowFrame.primary({
    super.key,
    required this.body,
    required this.closePolicy,
    this.title,
    this.captionBuilder,
    this.wordmark,
    this.wordmarkMinWidth,
    this.captionHeight = kBarHeight,
    this.captionBottomBorder = true,
    this.chrome = const ManagerWindowControlHost(),
  })  : assert(title != null || captionBuilder != null),
        assert(captionHeight > 0),
        role = WindowChromeRole.primary,
        ownedConfiguration = null;

  /// A contextual window whose visible identity is structurally owned by
  /// Covalence's secondary role. This constructor has no wordmark slot, so an
  /// address book or settings window cannot repeat `ASTROLABE` by accident.
  AstrolabeWindowFrame.secondary({
    super.key,
    required this.body,
    required this.title,
    required String nativeTitle,
    required String nativeKey,
    required Size size,
    required Size minimumSize,
    required bool dark,
    this.closePolicy = AstrolabeWindowClosePolicy.close,
    this.chrome = const NativeWindowControlHost(),
  })  : captionBuilder = null,
        wordmark = null,
        wordmarkMinWidth = null,
        captionHeight = kBarHeight,
        captionBottomBorder = true,
        role = WindowChromeRole.secondary,
        ownedConfiguration = OwnedWindowConfiguration(
          key: nativeKey,
          title: nativeTitle,
          size: size,
          minimumSize: minimumSize,
          dark: dark,
        );

  final Widget body;
  final AstrolabeWindowClosePolicy closePolicy;
  final WindowChromeRole role;
  final String? title;
  final AstrolabeCaptionBuilder? captionBuilder;
  final OwnedWindowConfiguration? ownedConfiguration;

  /// Optional interactive replacement for the default wordmark. The primary
  /// client uses it as its application-menu trigger; secondary windows keep
  /// the plain label.
  final Widget? wordmark;

  /// When null, the wordmark is always shown. Primary chrome may hide it at a
  /// narrow breakpoint to preserve its navigation targets.
  final double? wordmarkMinWidth;

  /// Main-client chrome is intentionally denser than a secondary window's
  /// standalone caption, so callers may choose the OS-facing tier's height.
  final double captionHeight;

  /// A two-tier header puts its separator under the navigation tier instead.
  final bool captionBottomBorder;

  /// How this window is moved and closed. The main engine uses
  /// [ManagerWindowControlHost]; an owned window uses
  /// [NativeWindowControlHost].
  final WindowControlHost chrome;

  @override
  State<AstrolabeWindowFrame> createState() => _AstrolabeWindowFrameState();
}

class _AstrolabeWindowFrameState extends State<AstrolabeWindowFrame>
    with WindowListener {
  bool _maximised = false;

  @override
  void initState() {
    super.initState();
    if (widget.chrome is ManagerWindowControlHost) {
      windowManager.addListener(this);
    }
    widget.chrome.isMaximized().then((maximised) {
      if (mounted) setState(() => _maximised = maximised);
    });
    final configuration = widget.ownedConfiguration;
    if (configuration != null) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted) unawaited(widget.chrome.configureOwned(configuration));
      });
    }
  }

  @override
  void dispose() {
    if (widget.chrome is ManagerWindowControlHost) {
      windowManager.removeListener(this);
    }
    super.dispose();
  }

  @override
  void onWindowMaximize() => setState(() => _maximised = true);

  @override
  void onWindowUnmaximize() => setState(() => _maximised = false);

  Future<void> _toggleMaximise() async {
    await widget.chrome.toggleMaximize();
    final maximised = await widget.chrome.isMaximized();
    if (mounted) setState(() => _maximised = maximised);
  }

  Future<void> _close() => switch (widget.closePolicy) {
        AstrolabeWindowClosePolicy.hide => widget.chrome.hide(),
        AstrolabeWindowClosePolicy.close => widget.chrome.close(),
      };

  @override
  Widget build(BuildContext context) {
    return ColoredBox(
      color: context.surface.l50,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (widget.role == WindowChromeRole.primary)
            _PrimaryCaption(
              title: widget.title,
              builder: widget.captionBuilder,
              wordmark: widget.wordmark,
              wordmarkMinWidth: widget.wordmarkMinWidth,
              height: widget.captionHeight,
              bottomBorder: widget.captionBottomBorder,
              maximised: _maximised,
              chrome: widget.chrome,
              onToggleMaximise: _toggleMaximise,
              closePolicy: widget.closePolicy,
              onClose: _close,
            )
          else
            WindowChrome.secondary(
              title: widget.title!,
              dragRegionBuilder: (context, child) => GestureDetector(
                behavior: HitTestBehavior.translucent,
                onPanStart: (_) => widget.chrome.startDragging(),
                onDoubleTap: _toggleMaximise,
                child: child,
              ),
              controlsBuilder: (context, policy) {
                assert(policy == WindowChromeControlPolicy.standard);
                return CaptionControls(
                  height: kBarHeight,
                  maximised: _maximised,
                  onMinimise: widget.chrome.minimize,
                  onToggleMaximise: _toggleMaximise,
                  onClose: _close,
                  closeTooltip: 'Close window',
                );
              },
            ),
          Expanded(child: widget.body),
        ],
      ),
    );
  }
}

class _PrimaryCaption extends StatelessWidget {
  const _PrimaryCaption({
    required this.title,
    required this.builder,
    required this.wordmark,
    required this.wordmarkMinWidth,
    required this.height,
    required this.bottomBorder,
    required this.maximised,
    required this.chrome,
    required this.onToggleMaximise,
    required this.closePolicy,
    required this.onClose,
  });

  final String? title;
  final AstrolabeCaptionBuilder? builder;
  final Widget? wordmark;
  final double? wordmarkMinWidth;
  final double height;
  final bool bottomBorder;
  final bool maximised;
  final WindowControlHost chrome;
  final Future<void> Function() onToggleMaximise;
  final AstrolabeWindowClosePolicy closePolicy;
  final Future<void> Function() onClose;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return t.box.height(
      // reason: the caption follows the operating system's target size rather
      // than the content rhythm used below it.
      TokenEscape.rawSize(height),
      child: Container(
        decoration: BoxDecoration(
          color: context.surface.l100,
          border:
              bottomBorder ? t.stroke.edge(bottom: context.border.l500) : null,
        ),
        child: Stack(
          fit: StackFit.expand,
          children: [
            // The drag region sits behind the caption's controls. Keeping it
            // out of their ancestor chain means its double-click recognizer
            // cannot delay a normal click on the app menu or window buttons.
            Positioned.fill(
              child: GestureDetector(
                behavior: HitTestBehavior.translucent,
                onPanStart: (_) => chrome.startDragging(),
                onDoubleTap: onToggleMaximise,
              ),
            ),
            LayoutBuilder(
              builder: (context, constraints) {
                final showWordmark = wordmarkMinWidth == null ||
                    constraints.maxWidth >= wordmarkMinWidth!;
                return Row(
                  children: [
                    t.gap.x(Space.xl3),
                    if (showWordmark) ...[
                      wordmark ??
                          IgnorePointer(
                            child: Text(
                              'ASTROLABE',
                              style: context.factLabelStyle.copyWith(
                                color: context.text.l950,
                                fontWeight: FontWeight.w700,
                                letterSpacing: 1.4,
                              ),
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
                        child: IgnorePointer(
                          child: Text(
                            title!,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: context.bodyStyle,
                          ),
                        ),
                      ),
                    ],
                    CaptionControls(
                      height: height,
                      maximised: maximised,
                      onMinimise: chrome.minimize,
                      onToggleMaximise: onToggleMaximise,
                      onClose: onClose,
                      closeTooltip:
                          closePolicy == AstrolabeWindowClosePolicy.hide
                              ? 'Close (it keeps serving in the tray)'
                              : 'Close window',
                    ),
                  ],
                );
              },
            ),
          ],
        ),
      ),
    );
  }
}
