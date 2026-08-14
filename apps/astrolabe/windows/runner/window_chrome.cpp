#include "window_chrome.h"

#include <windows.h>

#include <flutter/encodable_value.h>
#include <flutter/flutter_view_controller.h>
#include <flutter/method_channel.h>
#include <flutter/standard_method_codec.h>

#include <memory>
#include <string>
#include <variant>

namespace {

// The one sub-window this app creates today (the address book). Checked with
// IsWindow before every use, so a closed window reads as "nothing to raise"
// rather than as a stale handle.
HWND g_sub_window = nullptr;

HWND RootOf(HWND window) {
  HWND root = GetAncestor(window, GA_ROOT);
  return root == nullptr ? window : root;
}

void HideSystemCaption(HWND window) {
  HWND root = RootOf(window);
  LONG style = GetWindowLong(root, GWL_STYLE);
  // Drop the system caption; keep the resize border for the Dart frame.
  style &= ~WS_CAPTION;
  SetWindowLong(root, GWL_STYLE, style);
  SetWindowPos(root, nullptr, 0, 0, 0, 0,
               SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
}

// SW_SHOW alone neither restores a minimised window nor raises an occluded
// one — which is exactly what a second summons means.
void Raise(HWND root) {
  ShowWindow(root, IsIconic(root) ? SW_RESTORE : SW_SHOW);
  SetForegroundWindow(root);
}

void HandleCall(HWND window, const flutter::MethodCall<>& call,
                std::unique_ptr<flutter::MethodResult<>> result) {
  HWND root = RootOf(window);
  const std::string& method = call.method_name();
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
    WINDOWPLACEMENT placement = {sizeof(placement)};
    GetWindowPlacement(root, &placement);
    ShowWindow(root, placement.showCmd == SW_SHOWMAXIMIZED ? SW_RESTORE
                                                           : SW_MAXIMIZE);
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
  if (method == "summon") {
    Raise(root);
    result->Success();
    return;
  }
  if (method == "set_title") {
    const auto* title = std::get_if<std::string>(call.arguments());
    if (title == nullptr) {
      result->Error("bad_args", "set_title takes a string");
      return;
    }
    int needed = MultiByteToWideChar(CP_UTF8, 0, title->c_str(), -1, nullptr, 0);
    std::wstring wide(needed > 0 ? static_cast<size_t>(needed) : 0, L'\0');
    if (needed > 0) {
      MultiByteToWideChar(CP_UTF8, 0, title->c_str(), -1, wide.data(), needed);
      SetWindowTextW(root, wide.c_str());
    }
    result->Success();
    return;
  }
  result->NotImplemented();
}

}  // namespace

void RegisterWindowChrome(flutter::FlutterViewController* controller) {
  if (controller == nullptr || controller->engine() == nullptr ||
      controller->view() == nullptr) {
    return;
  }
  HWND window = controller->view()->GetNativeWindow();
  g_sub_window = RootOf(window);
  HideSystemCaption(window);
  auto channel = std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
      controller->engine()->messenger(), "astrolabe/window_chrome",
      &flutter::StandardMethodCodec::GetInstance());
  channel->SetMethodCallHandler(
      [window](const flutter::MethodCall<>& call,
               std::unique_ptr<flutter::MethodResult<>> result) {
        HandleCall(window, call, std::move(result));
      });
  // The channel must outlive the handler. The engine owns the messenger;
  // leaking the unique_ptr for the life of the process is the same cost
  // every other plugin pays, and a book window is one of them.
  channel.release();
}

void RegisterWindowSummon(flutter::FlutterViewController* controller) {
  if (controller == nullptr || controller->engine() == nullptr) {
    return;
  }
  auto channel =
      std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
          controller->engine()->messenger(), "astrolabe/window_summon",
          &flutter::StandardMethodCodec::GetInstance());
  channel->SetMethodCallHandler(
      [](const flutter::MethodCall<>& call,
         std::unique_ptr<flutter::MethodResult<>> result) {
        if (call.method_name() == "summon_book") {
          bool raised = g_sub_window != nullptr && IsWindow(g_sub_window);
          if (raised) {
            Raise(g_sub_window);
          }
          result->Success(flutter::EncodableValue(raised));
          return;
        }
        result->NotImplemented();
      });
  channel.release();
}
