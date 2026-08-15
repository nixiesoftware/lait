import Cocoa
import FlutterMacOS

/// The native half of Astrolabe's own window chrome — the macOS counterpart of
/// `windows/runner/window_chrome.cpp`.
///
/// Two channels, and the split is the same one that file describes:
///
///   * `astrolabe/window_chrome` is registered on an **owned** window's engine.
///     `window_manager` is a process singleton registered only on the main
///     engine, so a sub-engine cannot ask it anything; this is what it asks
///     instead.
///   * `astrolabe/window_host` is registered on the main engine, and exists for
///     `summon_owned` — raising a window that already exists rather than
///     opening a second one.
///
/// What is deliberately *not* ported:
///
///   * The owner relationship. Windows' `GWLP_HWNDPARENT` gives a window that
///     stays above its owner and minimises with it while still being dragged
///     anywhere. macOS's nearest equivalent, `addChildWindow`, also drags the
///     child whenever the parent moves — which is a different window, not a
///     smaller difference. An owned Astrolabe window is an ordinary top-level
///     window here.
///   * `WM_NCCALCSIZE` and the manual resize hit-test. They exist on Windows to
///     take the non-client strip back from DWM without giving up the standard
///     frame. macOS hands the whole window to the content view through
///     `.fullSizeContentView` and keeps its own resize edges, so there is
///     nothing to reclaim and nothing to re-implement.
enum AstrolabeWindowChrome {
  /// Every owned window that has named itself, by the key Dart configured it
  /// with. `summon_owned` reads this; nothing else does.
  private static var ownedByKey: [String: NSWindow] = [:]

  /// The main window, for centring an owned window on it.
  ///
  /// Weak: a strong reference here would keep the main window alive past its
  /// own close, and the only question ever asked of it is "where are you".
  private static weak var mainWindow: NSWindow?

  /// Whether an owned window has already been given its opening geometry.
  /// Keyed by window number rather than held on the window, because
  /// `desktop_multi_window` owns the `NSWindow` subclass and this file does not.
  private static var configured: Set<Int> = []

  /// Whether an owned window may be zoomed at all, by window number. Absent
  /// means yes — the pre-existing contract, so a caller that sends no
  /// `maximizable` key keeps its behaviour.
  private static var maximisable: [Int: Bool] = [:]

  // MARK: - Registration

  /// Install the host channel on the main engine.
  static func registerHost(_ controller: FlutterViewController) {
    mainWindow = controller.view.window

    let channel = FlutterMethodChannel(
      name: "astrolabe/window_host",
      binaryMessenger: controller.engine.binaryMessenger)
    channel.setMethodCallHandler { call, result in
      guard call.method == "summon_owned" else {
        result(FlutterMethodNotImplemented)
        return
      }
      guard let key = call.arguments as? String, let window = ownedByKey[key] else {
        result(false)
        return
      }
      raise(window)
      result(true)
    }
  }

  /// Install the chrome channel on an owned window's engine, and give the
  /// window the shape every Astrolabe surface has.
  ///
  /// Called from `desktop_multi_window`'s window-created callback, which fires
  /// before the sub-engine runs its entrypoint — so the channel is in place by
  /// the time the first frame's `configure_owned` arrives. Dart retries a few
  /// times anyway; see `NativeWindowControlHost.configureOwned`.
  static func registerOwned(_ controller: FlutterViewController) {
    guard let window = controller.view.window else { return }

    applyOwnedWindowPolicy(window)
    window.title = "Astrolabe"

    let channel = FlutterMethodChannel(
      name: "astrolabe/window_chrome",
      binaryMessenger: controller.engine.binaryMessenger)
    channel.setMethodCallHandler { [weak window] call, result in
      guard let window else {
        result(FlutterError(code: "gone", message: "the window has closed", details: nil))
        return
      }
      handle(call, on: window, result: result)
    }
  }

  // MARK: - Policy

  /// The frame Astrolabe draws its own caption into.
  ///
  /// `.fullSizeContentView` with a transparent, title-less titlebar is the
  /// macOS spelling of `window_manager`'s `TitleBarStyle.hidden`, which is what
  /// the main window already uses — so both windows in the process reach the
  /// same shape by the same means.
  ///
  /// The traffic lights stay. They are the window controls on this platform:
  /// at the leading edge, drawn by the system, in the place a person's muscle
  /// memory already points. The Dart caption draws no cluster opposite them
  /// (`systemDrawsWindowControls` in `shell/window.dart`), so there is still
  /// exactly one set — the difference from Windows is which side owns it, not
  /// how many there are.
  private static func applyOwnedWindowPolicy(_ window: NSWindow) {
    window.styleMask.insert(.fullSizeContentView)
    window.titleVisibility = .hidden
    window.titlebarAppearsTransparent = true
    window.isMovableByWindowBackground = false
  }

  private static func raise(_ window: NSWindow) {
    if window.isMiniaturized { window.deminiaturize(nil) }
    window.makeKeyAndOrderFront(nil)
    NSApp.activate(ignoringOtherApps: true)
  }

  // MARK: - Calls

  private static func handle(
    _ call: FlutterMethodCall, on window: NSWindow, result: @escaping FlutterResult
  ) {
    switch call.method {
    case "configure_owned":
      guard let values = call.arguments as? [String: Any],
        configureOwned(window, values)
      else {
        result(
          FlutterError(
            code: "bad_args", message: "configure_owned takes a window configuration",
            details: nil))
        return
      }
      result(nil)

    case "start_drag":
      // `currentEvent` is the mouse-drag that Flutter's pan recogniser is
      // reacting to. Without one there is nothing to hand AppKit, and asking
      // anyway throws.
      if let event = window.currentEvent {
        window.performDrag(with: event)
      }
      result(nil)

    case "minimize":
      window.miniaturize(nil)
      result(nil)

    case "toggle_maximize":
      // The Dart chrome of a non-maximisable window draws no control that
      // reaches here; this guard covers the summons that arrives anyway, so a
      // stale caller restores at most and never zooms.
      if window.isZoomed || maximisable[window.windowNumber] != false {
        window.zoom(nil)
      }
      result(nil)

    case "is_maximized":
      result(window.isZoomed)

    case "hide":
      window.orderOut(nil)
      result(nil)

    case "close":
      window.close()
      result(nil)

    case "set_title":
      guard let title = call.arguments as? String else {
        result(FlutterError(code: "bad_args", message: "set_title takes a string", details: nil))
        return
      }
      window.title = title
      result(nil)

    default:
      result(FlutterMethodNotImplemented)
    }
  }

  private static func configureOwned(_ window: NSWindow, _ values: [String: Any]) -> Bool {
    guard let key = values["key"] as? String,
      let title = values["title"] as? String,
      let width = values["width"] as? Double,
      let height = values["height"] as? Double,
      let minimumWidth = values["minimumWidth"] as? Double,
      let minimumHeight = values["minimumHeight"] as? Double,
      let dark = values["dark"] as? Bool
    else {
      return false
    }

    ownedByKey[key] = window

    // Points, not pixels: AppKit's frames are already in the logical unit Dart
    // measured in, so there is no counterpart to the Windows side's DPI scaling.
    window.minSize = NSSize(width: minimumWidth, height: minimumHeight)

    // A width ceiling is the whole portrait enforcement — every sizing path
    // (an edge drag, a zoom, a tiling gesture) is clamped through `maxSize`,
    // rather than being corrected after the gesture has already drawn.
    let maximumWidth = values["maximumWidth"] as? Double
    window.maxSize = NSSize(
      width: maximumWidth.map { max($0, minimumWidth) } ?? .greatestFiniteMagnitude,
      height: .greatestFiniteMagnitude)

    let canMaximise = values["maximizable"] as? Bool ?? true
    maximisable[window.windowNumber] = canMaximise
    if !canMaximise {
      // Removing the behaviour is what disarms the green button's full-screen
      // route and the double-click-to-zoom gesture; not drawing a control in
      // the Dart caption would leave both open.
      window.collectionBehavior.insert(.fullScreenNone)
      // And the button itself goes, rather than being drawn dead: absence is
      // the honest shape for a capability the window refuses, which is the
      // same rule the Dart caption follows when it draws no maximise control.
      window.standardWindowButton(.zoomButton)?.isHidden = true
      if window.isZoomed { window.zoom(nil) }
    }

    window.title = title
    window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)

    if !configured.contains(window.windowNumber) {
      window.setFrame(openingFrame(width: width, height: height), display: true)
      configured.insert(window.windowNumber)
    }

    raise(window)
    return true
  }

  /// Centred on the main window, then pulled back inside the screen it landed
  /// on. A window centred on a main window near an edge would otherwise open
  /// half off-screen — the same clamp the Windows side applies against the
  /// monitor's work area.
  private static func openingFrame(width: Double, height: Double) -> NSRect {
    let anchor = mainWindow?.frame ?? NSScreen.main?.visibleFrame ?? NSRect(
      x: 0, y: 0, width: width, height: height)
    var frame = NSRect(
      x: anchor.midX - width / 2,
      y: anchor.midY - height / 2,
      width: width,
      height: height)

    let screen =
      NSScreen.screens.first { $0.frame.intersects(anchor) } ?? NSScreen.main
    if let visible = screen?.visibleFrame {
      frame.origin.x = min(
        max(frame.minX, visible.minX), max(visible.minX, visible.maxX - width))
      frame.origin.y = min(
        max(frame.minY, visible.minY), max(visible.minY, visible.maxY - height))
    }
    return frame
  }
}
