import Cocoa
import FlutterMacOS
import desktop_multi_window
import window_manager

class MainFlutterWindow: NSWindow {
  override func awakeFromNib() {
    let flutterViewController = FlutterViewController()
    let windowFrame = self.frame
    self.contentViewController = flutterViewController
    self.setFrame(windowFrame, display: true)

    RegisterGeneratedPlugins(registry: flutterViewController)

    // An owned window's engine is not the main engine, so nothing the generated
    // registrant did applies to it. `desktop_multi_window` registers itself for
    // the sub-engine as it creates it (see `MultiWindowManager.CreateWindow`),
    // which is the whole of `windows/runner/engine_plugins.cpp` — tray_manager
    // and window_manager are process singletons and stay on the main engine
    // either way. What is left is this app's own chrome channel.
    FlutterMultiWindowPlugin.setOnWindowCreatedCallback { controller in
      AstrolabeWindowChrome.registerOwned(controller)
    }
    AstrolabeWindowChrome.registerHost(flutterViewController)

    // The client does not maximise, and here that has to be said to the
    // button. `window_manager`'s `setMaximizable` stores the flag on macOS and
    // reads it back — nothing else — so a Dart-side refusal would leave the
    // system's own zoom control live and working. The traffic lights are the
    // system's to draw; this disables the one of the three we refuse.
    self.standardWindowButton(.zoomButton)?.isEnabled = false

    super.awakeFromNib()
  }

  /// `window_manager` hides the window until Dart calls `show()`, and reaches
  /// that state through this override.
  override public func order(_ place: NSWindow.OrderingMode, relativeTo otherWin: Int) {
    super.order(place, relativeTo: otherWin)
    hiddenWindowAtLaunch()
  }
}
