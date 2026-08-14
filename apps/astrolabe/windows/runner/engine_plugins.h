#ifndef RUNNER_ENGINE_PLUGINS_H_
#define RUNNER_ENGINE_PLUGINS_H_

#include <flutter/plugin_registry.h>

// Plugins an owned window's engine may see. Not the generated registrant:
// that one also registers the tray and window_manager.
void RegisterOwnedWindowEnginePlugins(flutter::PluginRegistry* registry);

#endif  // RUNNER_ENGINE_PLUGINS_H_
