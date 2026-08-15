#ifndef RUNNER_WINDOW_CHROME_H_
#define RUNNER_WINDOW_CHROME_H_

#include <flutter/flutter_view_controller.h>

// Registers native ownership, gap-free non-client policy and the method
// channel an owned sub-engine uses instead of window_manager.
void RegisterOwnedWindowChrome(flutter::FlutterViewController* controller);

// Registers the main engine's host channel. The plugin's per-window channel
// answers only show/hide natively, so restoring and raising is the runner's
// job.
void RegisterWindowHost(flutter::FlutterViewController* controller);

#endif  // RUNNER_WINDOW_CHROME_H_
