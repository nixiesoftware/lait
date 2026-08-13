#ifndef RUNNER_WINDOW_CHROME_H_
#define RUNNER_WINDOW_CHROME_H_

#include <flutter/flutter_view_controller.h>

// Registers the method channel a sub-engine uses instead of window_manager.
// Also strips the system caption so the Dart frame can draw its own.
void RegisterWindowChrome(flutter::FlutterViewController* controller);

#endif  // RUNNER_WINDOW_CHROME_H_
