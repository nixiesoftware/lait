import Cocoa
import FlutterMacOS

@main
class AppDelegate: FlutterAppDelegate {
  /// Closing is not stopping.
  ///
  /// The main window's close policy is `hide` — it goes away and the client
  /// keeps supervising, which is what `Quit` in the application menu is for
  /// and the red traffic light is not. Terminating on the last closed window
  /// would let closing an owned window (the address book, a World's settings)
  /// stop everything while the main window was merely put away.
  override func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    return false
  }

  /// The way back in, once the main window has been put away: the Dock icon.
  /// `window_manager` ordered it out rather than closing it, so nothing else
  /// would bring it back.
  override func applicationShouldHandleReopen(
    _ sender: NSApplication, hasVisibleWindows flag: Bool
  ) -> Bool {
    guard !flag else { return true }
    for window in sender.windows where window is MainFlutterWindow {
      if window.isMiniaturized { window.deminiaturize(nil) }
      window.makeKeyAndOrderFront(self)
    }
    return true
  }

  override func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    return true
  }
}
