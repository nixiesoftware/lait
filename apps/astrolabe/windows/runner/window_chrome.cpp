#include "window_chrome.h"

#include <dwmapi.h>
#include <windows.h>
#include <windowsx.h>

#include <flutter/encodable_value.h>
#include <flutter/flutter_view_controller.h>
#include <flutter/method_channel.h>
#include <flutter/standard_method_codec.h>

#include <algorithm>
#include <cmath>
#include <memory>
#include <string>
#include <unordered_map>
#include <variant>

#ifndef DWMWA_USE_IMMERSIVE_DARK_MODE
#define DWMWA_USE_IMMERSIVE_DARK_MODE 20
#endif

#ifndef DWMWA_BORDER_COLOR
#define DWMWA_BORDER_COLOR 34
#endif

namespace {

constexpr COLORREF kNoDwmBorder = 0xFFFFFFFE;

struct OwnedWindowState {
  WNDPROC original_proc = nullptr;
  std::string key;
  int minimum_width = 0;
  int minimum_height = 0;
  // 0 means unbounded. A portrait-locked window pairs a ceiling here with a
  // minimum height at least as large, so no track size the OS can grant is
  // landscape.
  int maximum_width = 0;
  bool maximizable = true;
  bool configured = false;
};

HWND g_main_window = nullptr;
std::unordered_map<HWND, OwnedWindowState> g_owned_window_states;
std::unordered_map<std::string, HWND> g_owned_windows_by_key;

HWND RootOf(HWND window) {
  HWND root = GetAncestor(window, GA_ROOT);
  return root == nullptr ? window : root;
}

std::wstring Utf8ToWide(const std::string& value) {
  const int needed =
      MultiByteToWideChar(CP_UTF8, 0, value.c_str(), -1, nullptr, 0);
  if (needed <= 0) {
    return std::wstring();
  }
  std::wstring wide(static_cast<size_t>(needed), L'\0');
  MultiByteToWideChar(CP_UTF8, 0, value.c_str(), -1, wide.data(), needed);
  return wide;
}

const flutter::EncodableValue* Find(const flutter::EncodableMap& values,
                                    const char* key) {
  const auto found = values.find(flutter::EncodableValue(key));
  return found == values.end() ? nullptr : &found->second;
}

const std::string* StringValue(const flutter::EncodableMap& values,
                               const char* key) {
  const auto* value = Find(values, key);
  return value == nullptr ? nullptr : std::get_if<std::string>(value);
}

const bool* BoolValue(const flutter::EncodableMap& values, const char* key) {
  const auto* value = Find(values, key);
  return value == nullptr ? nullptr : std::get_if<bool>(value);
}

const double* DoubleValue(const flutter::EncodableMap& values,
                          const char* key) {
  const auto* value = Find(values, key);
  return value == nullptr ? nullptr : std::get_if<double>(value);
}

int ScaleForWindow(HWND window, double logical) {
  const UINT dpi = GetDpiForWindow(window);
  return static_cast<int>(std::lround(logical * dpi / USER_DEFAULT_SCREEN_DPI));
}

void ApplyDwmPolicy(HWND window, bool dark) {
  const BOOL use_dark = dark ? TRUE : FALSE;
  DwmSetWindowAttribute(window, DWMWA_USE_IMMERSIVE_DARK_MODE, &use_dark,
                        sizeof(use_dark));
  // Windows 11 understands this attribute; older releases return
  // E_INVALIDARG. The full-client NCCALCSIZE policy below remains the actual
  // white-gap fix on every supported release.
  DwmSetWindowAttribute(window, DWMWA_BORDER_COLOR, &kNoDwmBorder,
                        sizeof(kNoDwmBorder));
}

LRESULT ResizeHitTest(HWND window, LPARAM lparam) {
  if (IsZoomed(window)) {
    return HTCLIENT;
  }

  const UINT dpi = GetDpiForWindow(window);
  const int edge_x = GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) +
                     GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
  const int edge_y = GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) +
                     GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
  const POINT cursor = {GET_X_LPARAM(lparam), GET_Y_LPARAM(lparam)};
  RECT bounds{};
  GetWindowRect(window, &bounds);

  const bool left = cursor.x < bounds.left + edge_x;
  const bool right = cursor.x >= bounds.right - edge_x;
  const bool top = cursor.y < bounds.top + edge_y;
  const bool bottom = cursor.y >= bounds.bottom - edge_y;

  if (top && left) return HTTOPLEFT;
  if (top && right) return HTTOPRIGHT;
  if (bottom && left) return HTBOTTOMLEFT;
  if (bottom && right) return HTBOTTOMRIGHT;
  if (left) return HTLEFT;
  if (right) return HTRIGHT;
  if (top) return HTTOP;
  if (bottom) return HTBOTTOM;
  return HTCLIENT;
}

LRESULT CALLBACK OwnedWindowProc(HWND window, UINT message, WPARAM wparam,
                                 LPARAM lparam) {
  auto found = g_owned_window_states.find(window);
  if (found == g_owned_window_states.end()) {
    return DefWindowProc(window, message, wparam, lparam);
  }
  WNDPROC original = found->second.original_proc;

  switch (message) {
    case WM_NCCALCSIZE:
      if (wparam != 0) {
        auto* params = reinterpret_cast<NCCALCSIZE_PARAMS*>(lparam);
        if (IsZoomed(window)) {
          HMONITOR monitor =
              MonitorFromRect(&params->rgrc[0], MONITOR_DEFAULTTONEAREST);
          MONITORINFO info{sizeof(info)};
          if (monitor != nullptr && GetMonitorInfo(monitor, &info)) {
            params->rgrc[0] = info.rcWork;
          }
        }
        // Leaving the restored rectangle untouched makes the Flutter client
        // fill the outer window. WS_THICKFRAME stays set for snap layouts and
        // resizing; WM_NCHITTEST supplies its edge targets explicitly.
        return 0;
      }
      break;
    case WM_NCHITTEST: {
      const LRESULT hit = ResizeHitTest(window, lparam);
      if (hit != HTCLIENT) return hit;
      break;
    }
    case WM_GETMINMAXINFO: {
      auto* limits = reinterpret_cast<MINMAXINFO*>(lparam);
      if (found->second.minimum_width > 0) {
        limits->ptMinTrackSize.x = found->second.minimum_width;
      }
      if (found->second.minimum_height > 0) {
        limits->ptMinTrackSize.y = found->second.minimum_height;
      }
      // Every sizing path — edge drag, snap, Win+arrows — is clamped through
      // this message, so a width ceiling here is the whole portrait
      // enforcement rather than a correction racing the gesture.
      if (found->second.maximum_width > 0) {
        limits->ptMaxTrackSize.x = std::max(found->second.maximum_width,
                                            found->second.minimum_width);
      }
      return 0;
    }
    case WM_NCACTIVATE:
      // No system caption exists to repaint. Returning TRUE also prevents DWM
      // from briefly reintroducing a light inactive strip.
      return TRUE;
    case WM_NCDESTROY: {
      const std::string key = found->second.key;
      const LRESULT result = CallWindowProc(original, window, message, wparam,
                                            lparam);
      if (!key.empty()) {
        const auto keyed = g_owned_windows_by_key.find(key);
        if (keyed != g_owned_windows_by_key.end() && keyed->second == window) {
          g_owned_windows_by_key.erase(keyed);
        }
      }
      g_owned_window_states.erase(window);
      return result;
    }
  }

  return CallWindowProc(original, window, message, wparam, lparam);
}

void InstallOwnedWindowPolicy(HWND window) {
  HWND root = RootOf(window);
  if (g_main_window != nullptr && IsWindow(g_main_window)) {
    // For a top-level window GWLP_HWNDPARENT assigns an owner, not a child
    // parent. It remains independently movable while following the main
    // window's minimize/lifetime behavior.
    SetWindowLongPtr(root, GWLP_HWNDPARENT,
                     reinterpret_cast<LONG_PTR>(g_main_window));
  }

  LONG style = GetWindowLong(root, GWL_STYLE);
  style &= ~WS_CAPTION;
  style |= WS_THICKFRAME;
  SetWindowLong(root, GWL_STYLE, style);

  if (g_owned_window_states.find(root) == g_owned_window_states.end()) {
    OwnedWindowState state;
    state.original_proc =
        reinterpret_cast<WNDPROC>(GetWindowLongPtr(root, GWLP_WNDPROC));
    g_owned_window_states.emplace(root, state);
    SetWindowLongPtr(root, GWLP_WNDPROC,
                     reinterpret_cast<LONG_PTR>(OwnedWindowProc));
  }

  ApplyDwmPolicy(root, true);
  SetWindowPos(root, nullptr, 0, 0, 0, 0,
               SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE |
                   SWP_FRAMECHANGED);
}

void Raise(HWND window) {
  ShowWindow(window, IsIconic(window) ? SW_RESTORE : SW_SHOW);
  SetForegroundWindow(window);
}

bool ConfigureOwnedWindow(HWND root, const flutter::EncodableMap& values) {
  const auto* key = StringValue(values, "key");
  const auto* title = StringValue(values, "title");
  const auto* width = DoubleValue(values, "width");
  const auto* height = DoubleValue(values, "height");
  const auto* minimum_width = DoubleValue(values, "minimumWidth");
  const auto* minimum_height = DoubleValue(values, "minimumHeight");
  const auto* dark = BoolValue(values, "dark");
  if (key == nullptr || title == nullptr || width == nullptr ||
      height == nullptr || minimum_width == nullptr ||
      minimum_height == nullptr || dark == nullptr) {
    return false;
  }

  auto found = g_owned_window_states.find(root);
  if (found == g_owned_window_states.end()) {
    return false;
  }
  OwnedWindowState& state = found->second;
  state.minimum_width = ScaleForWindow(root, *minimum_width);
  state.minimum_height = ScaleForWindow(root, *minimum_height);
  // Optional keys: absent means unbounded and maximizable, the pre-existing
  // contract, so older callers keep their behavior unchanged.
  const auto* maximum_width = DoubleValue(values, "maximumWidth");
  state.maximum_width =
      maximum_width == nullptr ? 0 : ScaleForWindow(root, *maximum_width);
  const auto* maximizable = BoolValue(values, "maximizable");
  state.maximizable = maximizable == nullptr ? true : *maximizable;
  state.key = *key;
  g_owned_windows_by_key[*key] = root;

  if (!state.maximizable) {
    // Removing the style is what disarms Win+Up, snap-assist's maximize tile,
    // and the top-edge drag gesture; undrawing the caption button alone would
    // leave all three routes open.
    LONG style = GetWindowLong(root, GWL_STYLE);
    if ((style & WS_MAXIMIZEBOX) != 0) {
      SetWindowLong(root, GWL_STYLE, style & ~WS_MAXIMIZEBOX);
      SetWindowPos(root, nullptr, 0, 0, 0, 0,
                   SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE |
                       SWP_FRAMECHANGED);
    }
    if (IsZoomed(root)) {
      ShowWindow(root, SW_RESTORE);
    }
  }

  const std::wstring wide_title = Utf8ToWide(*title);
  SetWindowTextW(root, wide_title.c_str());
  ApplyDwmPolicy(root, *dark);

  if (!state.configured) {
    const int target_width = ScaleForWindow(root, *width);
    const int target_height = ScaleForWindow(root, *height);
    RECT anchor{};
    if (g_main_window == nullptr || !GetWindowRect(g_main_window, &anchor)) {
      anchor = {0, 0, target_width, target_height};
    }

    HMONITOR monitor =
        MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST);
    MONITORINFO info{sizeof(info)};
    RECT work = anchor;
    if (monitor != nullptr && GetMonitorInfo(monitor, &info)) {
      work = info.rcWork;
    }

    int left = anchor.left +
               ((anchor.right - anchor.left) - target_width) / 2;
    int top = anchor.top +
              ((anchor.bottom - anchor.top) - target_height) / 2;
    const int work_left = static_cast<int>(work.left);
    const int work_top = static_cast<int>(work.top);
    const int work_right = static_cast<int>(work.right);
    const int work_bottom = static_cast<int>(work.bottom);
    left = std::clamp(
        left, work_left, std::max(work_left, work_right - target_width));
    top = std::clamp(
        top, work_top, std::max(work_top, work_bottom - target_height));
    SetWindowPos(root, nullptr, left, top, target_width, target_height,
                 SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
    state.configured = true;
  }

  Raise(root);
  return true;
}

void HandleCall(HWND window, const flutter::MethodCall<>& call,
                std::unique_ptr<flutter::MethodResult<>> result) {
  HWND root = RootOf(window);
  const std::string& method = call.method_name();
  if (method == "configure_owned") {
    const auto* values = std::get_if<flutter::EncodableMap>(call.arguments());
    if (values == nullptr || !ConfigureOwnedWindow(root, *values)) {
      result->Error("bad_args", "configure_owned takes a window configuration");
    } else {
      result->Success();
    }
    return;
  }
  if (method == "start_drag") {
    ReleaseCapture();
    SendMessage(root, WM_NCLBUTTONDOWN, HTCAPTION, 0);
    result->Success();
    return;
  }
  if (method == "minimize") {
    ShowWindow(root, SW_MINIMIZE);
    result->Success();
    return;
  }
  if (method == "toggle_maximize") {
    // The Dart chrome of a non-maximizable window draws no control that
    // reaches here; this guard covers the summons that arrives anyway, so a
    // stale caller restores at most and never maximizes.
    const auto state = g_owned_window_states.find(root);
    const bool maximizable =
        state == g_owned_window_states.end() || state->second.maximizable;
    WINDOWPLACEMENT placement = {sizeof(placement)};
    GetWindowPlacement(root, &placement);
    if (placement.showCmd == SW_SHOWMAXIMIZED) {
      ShowWindow(root, SW_RESTORE);
    } else if (maximizable) {
      ShowWindow(root, SW_MAXIMIZE);
    }
    result->Success();
    return;
  }
  if (method == "is_maximized") {
    WINDOWPLACEMENT placement = {sizeof(placement)};
    GetWindowPlacement(root, &placement);
    result->Success(flutter::EncodableValue(placement.showCmd ==
                                            SW_SHOWMAXIMIZED));
    return;
  }
  if (method == "hide") {
    ShowWindow(root, SW_HIDE);
    result->Success();
    return;
  }
  if (method == "close") {
    PostMessage(root, WM_CLOSE, 0, 0);
    result->Success();
    return;
  }
  if (method == "set_title") {
    const auto* title = std::get_if<std::string>(call.arguments());
    if (title == nullptr) {
      result->Error("bad_args", "set_title takes a string");
      return;
    }
    const std::wstring wide = Utf8ToWide(*title);
    SetWindowTextW(root, wide.c_str());
    result->Success();
    return;
  }
  result->NotImplemented();
}

}  // namespace

void RegisterOwnedWindowChrome(flutter::FlutterViewController* controller) {
  if (controller == nullptr || controller->engine() == nullptr ||
      controller->view() == nullptr) {
    return;
  }
  HWND root = RootOf(controller->view()->GetNativeWindow());
  InstallOwnedWindowPolicy(root);
  SetWindowTextW(root, L"Astrolabe");

  auto channel =
      std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
          controller->engine()->messenger(), "astrolabe/window_chrome",
          &flutter::StandardMethodCodec::GetInstance());
  channel->SetMethodCallHandler(
      [root](const flutter::MethodCall<>& call,
             std::unique_ptr<flutter::MethodResult<>> result) {
        HandleCall(root, call, std::move(result));
      });
  // The engine owns the messenger; the channel must live for the same span.
  channel.release();
}

void RegisterWindowHost(flutter::FlutterViewController* controller) {
  if (controller == nullptr || controller->engine() == nullptr ||
      controller->view() == nullptr) {
    return;
  }
  g_main_window = RootOf(controller->view()->GetNativeWindow());
  auto channel =
      std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
          controller->engine()->messenger(), "astrolabe/window_host",
          &flutter::StandardMethodCodec::GetInstance());
  channel->SetMethodCallHandler(
      [](const flutter::MethodCall<>& call,
         std::unique_ptr<flutter::MethodResult<>> result) {
        if (call.method_name() == "summon_owned") {
          const auto* key = std::get_if<std::string>(call.arguments());
          bool raised = false;
          if (key != nullptr) {
            const auto found = g_owned_windows_by_key.find(*key);
            if (found != g_owned_windows_by_key.end() &&
                IsWindow(found->second)) {
              Raise(found->second);
              raised = true;
            }
          }
          result->Success(flutter::EncodableValue(raised));
          return;
        }
        result->NotImplemented();
      });
  channel.release();
}
