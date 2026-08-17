/// The one window contract for every Astrolabe-owned desktop surface.
///
/// New windows get both their native configuration and their visible frame
/// here. Keeping those halves together prevents a secondary surface from
/// accidentally falling back to an operating-system title bar that disagrees
/// with the active Astrolabe theme.
///
/// ## What the system already owns, this client does not draw
///
/// The frame is one shape on both platforms, but two facts about the host
/// decide what fills it — [systemDrawsWindowControls] and
/// [systemCarriesApplicationMenu]. Windows gives an undecorated window nothing
/// at all, so the caption paints its own cluster at the trailing corner and
/// carries the wordmark that opens the application menu. macOS keeps its
/// traffic lights under `.fullSizeContentView` and gives every application a
/// menu bar at the top of the screen; drawing either again in the window would
/// be a second set of controls disagreeing with the first about what close
/// means, and a second application menu disagreeing with the first about what
/// the settings are.
library;

import 'dart:async';

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/foundation.dart' show defaultTargetPlatform;
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
    this.maximumWidth,
    this.maximisable = true,
  }) : assert(
          maximisable || maximumWidth != null,
          'a window that cannot maximise needs a width ceiling, or dragging '
          'an edge reintroduces the full-screen shape the flag removed',
        );

  final String key;
  final String title;
  final Size size;
  final Size minimumSize;
  final bool dark;

  /// The widest the window may be dragged, in logical pixels. When it is no
  /// larger than [minimumSize]'s height, the window can never leave portrait —
  /// the invariant is arithmetic, not a resize-time correction.
  final double? maximumWidth;

  /// Whether the OS may ever show this window maximised. `false` removes the
  /// native maximise style, so Win+Up and a top-edge drag do nothing rather
  /// than being undone after the fact.
  final bool maximisable;

  Map<String, Object> toMap() => {
        'key': key,
        'title': title,
        'width': size.width,
        'height': size.height,
        'minimumWidth': minimumSize.width,
        'minimumHeight': minimumSize.height,
        'dark': dark,
        'maximizable': maximisable,
        if (maximumWidth != null) 'maximumWidth': maximumWidth!,
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

  /// Cover the display, or stop covering it.
  ///
  /// Distinct from [toggleMaximize], and the distinction is the whole reason
  /// this exists: a maximised window keeps its frame and leaves the taskbar
  /// over it, which is a large window rather than a screen. Big Picture needs
  /// the display, and it is an assertion rather than a toggle so that entering
  /// twice is entering once.
  Future<void> setFullScreen(bool full);

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
  Future<void> setFullScreen(bool full) => windowManager.setFullScreen(full);

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

  /// Sub-engine windows do not present.
  ///
  /// Big Picture belongs to the primary window, which is the one carrying the
  /// client's identity and its Space. A book or settings window covering the
  /// display would be a second screen nobody chose, so this refuses by doing
  /// nothing rather than growing a runner method to support it.
  @override
  Future<void> setFullScreen(bool full) async {}

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

/// Whether the operating system draws this window's own controls.
///
/// macOS does, at the leading edge, and keeps doing it when the title bar is
/// merely hidden. Windows draws nothing on an undecorated window, which is why
/// [CaptionControls] exists at all.
bool get systemDrawsWindowControls =>
    defaultTargetPlatform == TargetPlatform.macOS;

/// Whether the operating system carries the application's own menu.
///
/// macOS puts it at the top of the screen, above every window and outside all
/// of them; that is where this client's name and its settings belong, so the
/// caption draws no wordmark. Windows has no such bar and the wordmark is the
/// only application menu there is.
bool get systemCarriesApplicationMenu =>
    defaultTargetPlatform == TargetPlatform.macOS;

/// What the system's own controls occupy at the leading edge.
///
/// AppKit lays the three buttons out from x=7 in 20-point steps at 14 points
/// wide, so the cluster ends at 61; the rest is the gap the system's own
/// windows leave before their first content. Nothing of ours may be drawn
/// inside it — a control under a traffic light is a control nobody can press.
const double kTrafficLightSpan = 78;

/// The band those controls are centred in: the standard title bar's height,
/// which is what AppKit keeps reserving once `.fullSizeContentView` has handed
/// the window to us. Content that must clear them clears this.
const double kTrafficLightBand = 28;

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
  Size? maximumSize,
  bool center = true,
}) {
  return WindowOptions(
    size: size,
    minimumSize: minimumSize,
    // Windows clamps `ptMaxTrackSize` from this, so the ceiling holds against
    // a corner drag and not merely against the maximise the window already
    // refuses.
    maximumSize: maximumSize,
    center: center,
    title: title,
    titleBarStyle: TitleBarStyle.hidden,
    // macOS keeps its traffic lights when the title bar is merely hidden, and
    // they are the set this window shows: at the leading edge, where every
    // other window on the machine keeps them, drawn by the system that owns
    // them. The caption draws none opposite them, so there is still exactly
    // one. Ignored on Windows, which has no such buttons to keep.
    windowButtonVisibility: systemDrawsWindowControls,
  );
}

/// Whether the client window may ever be maximised.
///
/// It may. This was `false`, and the reasoning was sound while it held: a
/// launcher filling a 4K display is a window of empty page around one card, so
/// the window refused the shape rather than being resized back out of it.
///
/// Big Picture ended that. A window that cannot leave its ceiling cannot become
/// a screen, and `maximumSize` clamps `ptMaxTrackSize` — so the ceiling fought
/// not just a maximise but the fullscreen the presentation surface needs. The
/// constraint was protecting the Library's proportions and paying for it with
/// the client's ability to be the thing REACH-31 calls a member screen.
///
/// The Library's proportions are now a layout concern, where they belong: it
/// centres its own content rather than relying on the window to be small.
///
/// One constant, read by both halves — the native configuration in `main` and
/// the chrome in the shell. Two literals would be two places to disagree, and
/// a caption offering maximise over a window that refuses it is the same
/// defect as the reverse.
const bool kClientMaximisable = true;

/// Shows a configured Astrolabe window without repeating platform setup at
/// each process entrypoint.
///
/// [maximisable] is applied before the window is ever shown, so no frame is
/// drawn wearing a control the window then loses.
Future<void> showAstrolabeWindow(
  WindowOptions options, {
  bool maximisable = true,
}) {
  return windowManager.waitUntilReadyToShow(
    options,
    () async {
      // On Windows this drops WS_MAXIMIZEBOX, which disarms Win+Up and
      // snap-assist's maximise tile as well as the button. On macOS
      // `window_manager` only records the flag, so the zoom control is
      // refused where it is drawn instead — see `MainFlutterWindow`.
      if (!maximisable) await windowManager.setMaximizable(false);
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
    this.captionTrailing,
    this.wordmark,
    this.wordmarkMinWidth,
    this.captionHeight = kBarHeight,
    this.captionBottomBorder = true,
    this.maximisable = true,
    this.chrome = const ManagerWindowControlHost(),
  })  : assert(title != null || captionBuilder != null),
        assert(captionHeight > 0),
        role = WindowChromeRole.primary,
        mergedCaption = false,
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
    double? maximumWidth,
    this.maximisable = true,
    this.mergedCaption = false,
    // A window whose chrome IS its tabs — a browser, a terminal, the chat —
    // draws them here, in the caption band beside the system controls, instead
    // of a title. When given, the title is not drawn.
    this.captionBuilder,
    this.closePolicy = AstrolabeWindowClosePolicy.close,
    this.chrome = const NativeWindowControlHost(),
  })  : captionTrailing = null,
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
          maximumWidth: maximumWidth,
          maximisable: maximisable,
        );

  final Widget body;
  final AstrolabeWindowClosePolicy closePolicy;
  final WindowChromeRole role;
  final String? title;
  final AstrolabeCaptionBuilder? captionBuilder;

  /// Utility controls that sit immediately before the window's own, at the
  /// trailing edge.
  ///
  /// A slot rather than something the caption builder right-aligns for itself:
  /// the builder is handed an [Expanded] region, so anything it pushes right
  /// lands at the end of *that* region, which is not the same as the end of
  /// the bar once a wordmark or a title has taken its share. Two things wanted
  /// the trailing edge and only one can have it by alignment.
  ///
  /// It never gets the corner. [CaptionControls] stays flush against it,
  /// because a maximised window's corner is the easiest pixel on the screen to
  /// hit and a cluster inset from it by even a point gives that up.
  final Widget? captionTrailing;

  final OwnedWindowConfiguration? ownedConfiguration;

  /// Whether this window offers maximise at all. When false, the caption
  /// draws no maximise control and the drag region's double-click does
  /// nothing — the same fact the native configuration enforces on the HWND,
  /// stated once here so the visible chrome cannot promise what the window
  /// refuses.
  final bool maximisable;

  /// Draw no caption band at all: the body owns the window's whole height
  /// and the window controls float over its top-right corner. For a window
  /// whose leading content IS its identity — the address book's canonical
  /// card — a band above it was chrome spending height on what the content
  /// already says. The caption-height strip across the top stays a native
  /// drag region, translucent over the body, so the window still moves like
  /// one. Secondary windows only.
  final bool mergedCaption;

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
      // A close this client never drew still has to mean what the drawn one
      // means. The red traffic light and `⌘W` arrive at the system's close,
      // and a window whose policy is `hide` would otherwise be stopped by them
      // while the drawn control merely put it away — one window with two
      // closes that disagree. Only where the system draws a close: on Windows
      // there is none to intercept, and refusing `Alt+F4` there would put the
      // window away with nothing yet built to bring it back.
      if (systemDrawsWindowControls &&
          widget.closePolicy == AstrolabeWindowClosePolicy.hide) {
        unawaited(windowManager.setPreventClose(true));
      }
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

  /// The system's close, refused above, arrives here instead. It applies the
  /// same policy [_close] does, because it is the same act.
  @override
  void onWindowClose() {
    if (widget.closePolicy == AstrolabeWindowClosePolicy.hide) {
      unawaited(widget.chrome.hide());
    }
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
    if (widget.mergedCaption) {
      return ColoredBox(
        color: context.surface.l50,
        child: Stack(
          children: [
            Positioned.fill(
              child: systemDrawsWindowControls
                  // The traffic lights float over the body's leading corner,
                  // and the body's first content is the canonical card — so
                  // the card starts below the band they are centred in rather
                  // than beneath them. Nothing else moves: the merge is still
                  // a merge, one band shorter.
                  //
                  // reason: the inset is the system's own title-bar height,
                  // not a step in this app's rhythm.
                  ? Padding(
                      padding:
                          TokenEscape.rawPadding(top: kTrafficLightBand),
                      child: widget.body,
                    )
                  : widget.body,
            ),
            // The drag region lies over the body's top strip but stays
            // translucent and claims only the pan (and the double-click,
            // where maximise exists), so the content beneath keeps every
            // gesture of its own — the same arena split the primary caption
            // draws, approached from the other side.
            Positioned(
              top: 0,
              left: 0,
              right: 0,
              height: widget.captionHeight,
              child: GestureDetector(
                behavior: HitTestBehavior.translucent,
                onPanStart: (_) => widget.chrome.startDragging(),
                onDoubleTap: widget.maximisable ? _toggleMaximise : null,
              ),
            ),
            if (!systemDrawsWindowControls)
              Positioned(
                top: 0,
                right: 0,
                child: CaptionControls(
                  height: widget.captionHeight,
                  maximised: _maximised,
                  onMinimise: widget.chrome.minimize,
                  onToggleMaximise: widget.maximisable ? _toggleMaximise : null,
                  onClose: _close,
                  closeTooltip:
                      widget.closePolicy == AstrolabeWindowClosePolicy.hide
                          ? 'Close (it keeps serving in the tray)'
                          : 'Close window',
                ),
              ),
          ],
        ),
      );
    }
    return ColoredBox(
      color: context.surface.l50,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (widget.role == WindowChromeRole.primary)
            _PrimaryCaption(
              title: widget.title,
              builder: widget.captionBuilder,
              trailing: widget.captionTrailing,
              wordmark: widget.wordmark,
              wordmarkMinWidth: widget.wordmarkMinWidth,
              height: widget.captionHeight,
              bottomBorder: widget.captionBottomBorder,
              maximised: _maximised,
              chrome: widget.chrome,
              onToggleMaximise: widget.maximisable ? _toggleMaximise : null,
              closePolicy: widget.closePolicy,
              onClose: _close,
            )
          // A secondary window that draws its own caption content — the chat's
          // browser-style tabs — uses the same caption widget with no wordmark,
          // so the tabs sit in the band beside the system controls rather than
          // below them.
          else if (widget.captionBuilder != null)
            _PrimaryCaption(
              title: null,
              builder: widget.captionBuilder,
              trailing: widget.captionTrailing,
              wordmark: null,
              wordmarkMinWidth: null,
              height: widget.captionHeight,
              bottomBorder: widget.captionBottomBorder,
              maximised: _maximised,
              chrome: widget.chrome,
              onToggleMaximise: widget.maximisable ? _toggleMaximise : null,
              closePolicy: widget.closePolicy,
              onClose: _close,
            )
          else
            WindowChrome.secondary(
              title: widget.title!,
              // The system's controls sit in the band's leading corner, so the
              // title starts clear of them instead of under them. On Windows
              // there is nothing there and nothing is ceded.
              //
              // reason: the reserve is AppKit's own cluster width, not a step
              // in this app's rhythm.
              leading: systemDrawsWindowControls
                  ? TokenEscape.rawGap(width: kTrafficLightSpan)
                  : null,
              dragRegionBuilder: (context, child) => GestureDetector(
                behavior: HitTestBehavior.translucent,
                onPanStart: (_) => widget.chrome.startDragging(),
                onDoubleTap: widget.maximisable ? _toggleMaximise : null,
                child: child,
              ),
              controlsBuilder: systemDrawsWindowControls
                  ? null
                  : (context, policy) {
                      assert(policy == WindowChromeControlPolicy.standard);
                      return CaptionControls(
                        height: kBarHeight,
                        maximised: _maximised,
                        onMinimise: widget.chrome.minimize,
                        onToggleMaximise:
                            widget.maximisable ? _toggleMaximise : null,
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
    required this.trailing,
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
  final Widget? trailing;
  final Widget? wordmark;
  final double? wordmarkMinWidth;
  final double height;
  final bool bottomBorder;
  final bool maximised;
  final WindowControlHost chrome;

  /// `null` where this window cannot maximise: the control is not drawn and
  /// the band's double-click does nothing. The primary caption reached its
  /// own maximise unconditionally while every primary window could, which
  /// left the flag true in the only place it was not read.
  final Future<void> Function()? onToggleMaximise;
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
                // On macOS the application menu is the screen's own, so the
                // window carries no wordmark to open one with — and the
                // corner it used to start in belongs to the traffic lights.
                final showWordmark = !systemCarriesApplicationMenu &&
                    (wordmarkMinWidth == null ||
                        constraints.maxWidth >= wordmarkMinWidth!);
                return Row(
                  children: [
                    if (systemDrawsWindowControls)
                      // reason: the reserve is AppKit's own cluster width,
                      // not a step in this app's rhythm.
                      TokenEscape.rawGap(width: kTrafficLightSpan)
                    else
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
                    // Trailing utilities, then the window's own controls, then
                    // the corner. On macOS the system's controls are at the
                    // *leading* edge, so this is simply the end of the bar.
                    if (trailing != null) trailing!,
                    if (!systemDrawsWindowControls)
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
