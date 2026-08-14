#ifndef RUNNER_WINDOW_CHROME_H_
#define RUNNER_WINDOW_CHROME_H_

#include <flutter/flutter_view_controller.h>

// Registers the method channel a sub-engine uses instead of window_manager.
// Also strips the system caption so the Dart frame can draw its own.
void RegisterWindowChrome(flutter::FlutterViewController* controller);

// Registers the main engine's summon channel. The plugin's per-window channel
// answers only show/hide natively, so raising a minimised or occluded book
// window is the runner's job: it remembers the sub-window's root HWND and
// restores + foregrounds it on request.
void RegisterWindowSummon(flutter::FlutterViewController* controller);

#endif  // RUNNER_WINDOW_CHROME_H_
