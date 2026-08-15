#include "engine_plugins.h"

#include <desktop_multi_window/desktop_multi_window_plugin.h>

void RegisterOwnedWindowEnginePlugins(flutter::PluginRegistry* registry) {
  // Exactly what an owned surface needs. tray_manager stays on the main engine
  // so a second window cannot plant a second tray icon. window_manager
  // is a process singleton and is not registered here.
  DesktopMultiWindowPluginRegisterWithRegistrar(
      registry->GetRegistrarForPlugin("DesktopMultiWindowPlugin"));
}
