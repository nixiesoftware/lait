#include "window_chrome.h"

#include <windows.h>

#include <flutter/flutter_view_controller.h>
#include <flutter/method_channel.h>
#include <flutter/standard_method_codec.h>

#include <memory>

namespace {

HWND RootOf(HWND window) {
  HWND root = GetAncestor(window, GA_ROOT);
  return root == nullptr ? window : root;
}

void HideSystemCaption(HWND window) {
  HWND root = RootOf(window);
  LONG style = GetWindowLong(root, GWL_STYLE);
  style &= ~(WS_CAPTION | WS_THICKFRAME);
  style |= WS_THICKFRAME;
  SetWindowLong(root, GWL_STYLE, style);
  SetWindowPos(root, nullptr, 0, 0, 0, 0,
               SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
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
  result->NotImplemented();
}

}  // namespace

void RegisterWindowChrome(flutter::FlutterViewController* controller) {
  if (controller == nullptr || controller->engine() == nullptr ||
      controller->view() == nullptr) {
    return;
  }
  HWND window = controller->view()->GetNativeWindow();
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
