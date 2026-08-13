#include "engine_plugins.h"

#include <desktop_multi_window/desktop_multi_window_plugin.h>

void RegisterBookEnginePlugins(flutter::PluginRegistry* registry) {
  // Exactly what the book needs. tray_manager stays on the main engine
  // so a second window cannot plant a second tray icon. window_manager
  // is a process singleton and is not registered here.
  DesktopMultiWindowPluginRegisterWithRegistrar(
      registry->GetRegistrarForPlugin("DesktopMultiWindowPlugin"));
}
