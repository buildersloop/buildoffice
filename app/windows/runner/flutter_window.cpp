#include "flutter_window.h"

#include <flutter/standard_method_codec.h>
#include <shobjidl.h>

#include <optional>
#include <string>

#include "flutter/generated_plugin_registrant.h"
#include "utils.h"

namespace {

// Shows the OS folder picker (IFileOpenDialog) as a modal dialog owned by
// |owner|. Returns S_OK and sets |folder| (UTF-8, absolute path) when a folder
// was chosen, S_FALSE when the user cancelled, or a failure HRESULT. Every COM
// object is released and the path string is freed with CoTaskMemFree before
// returning. The path is not checked here; the Rust core checks it.
HRESULT PickFolder(HWND owner, std::string& folder) {
  IFileOpenDialog* dialog = nullptr;
  HRESULT hr = ::CoCreateInstance(CLSID_FileOpenDialog, nullptr,
                                  CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&dialog));
  if (FAILED(hr)) {
    return hr;
  }
  FILEOPENDIALOGOPTIONS options = 0;
  hr = dialog->GetOptions(&options);
  if (SUCCEEDED(hr)) {
    // Folders only, file system items only, existing items only. Do not
    // change the current directory and do not add the folder to Recent.
    hr = dialog->SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM |
                            FOS_PATHMUSTEXIST | FOS_NOCHANGEDIR |
                            FOS_DONTADDTORECENT);
  }
  if (SUCCEEDED(hr)) {
    hr = dialog->Show(owner);
  }
  IShellItem* item = nullptr;
  if (SUCCEEDED(hr)) {
    hr = dialog->GetResult(&item);
  }
  if (SUCCEEDED(hr)) {
    PWSTR path = nullptr;
    hr = item->GetDisplayName(SIGDN_FILESYSPATH, &path);
    if (SUCCEEDED(hr)) {
      folder = Utf8FromUtf16(path);
      if (folder.empty()) {
        hr = E_FAIL;
      }
    }
    ::CoTaskMemFree(path);
    item->Release();
  }
  dialog->Release();
  if (hr == HRESULT_FROM_WIN32(ERROR_CANCELLED)) {
    return S_FALSE;
  }
  return hr;
}

// Sets |flag| to true while it lives and sets it back to false on every exit
// (normal return, early return, exception).
class ScopedFlag {
 public:
  explicit ScopedFlag(bool& flag) : flag_(flag) { flag_ = true; }
  ~ScopedFlag() { flag_ = false; }
  ScopedFlag(const ScopedFlag&) = delete;
  ScopedFlag& operator=(const ScopedFlag&) = delete;

 private:
  bool& flag_;
};

}  // namespace

FlutterWindow::FlutterWindow(const flutter::DartProject& project)
    : project_(project) {}

FlutterWindow::~FlutterWindow() {}

bool FlutterWindow::OnCreate() {
  if (!Win32Window::OnCreate()) {
    return false;
  }

  RECT frame = GetClientArea();

  // The size here must match the window dimensions to avoid unnecessary surface
  // creation / destruction in the startup path.
  flutter_controller_ = std::make_unique<flutter::FlutterViewController>(
      frame.right - frame.left, frame.bottom - frame.top, project_);
  // Ensure that basic setup of the controller was successful.
  if (!flutter_controller_->engine() || !flutter_controller_->view()) {
    return false;
  }
  RegisterPlugins(flutter_controller_->engine());

  // The folder picker channel (only "pickFolder"). The reply is the chosen
  // path, null when cancelled, or the error PICK_FAILED (without any OS text).
  // PICK_FAILED is also the reply to a "pickFolder" that arrives while a
  // dialog is already open.
  folder_picker_channel_ =
      std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
          flutter_controller_->engine()->messenger(),
          "buildoffice/folder_picker",
          &flutter::StandardMethodCodec::GetInstance());
  folder_picker_channel_->SetMethodCallHandler(
      [this](const flutter::MethodCall<flutter::EncodableValue>& call,
             std::unique_ptr<flutter::MethodResult<flutter::EncodableValue>>
                 result) {
        if (call.method_name() != "pickFolder") {
          result->NotImplemented();
          return;
        }
        // The dialog is modal but pumps messages, so a second "pickFolder" can
        // arrive while it is open. Refuse it (never open two dialogs).
        if (picking_folder_) {
          result->Error("PICK_FAILED");
          return;
        }
        ScopedFlag picking(picking_folder_);
        std::string folder;
        const HRESULT hr = PickFolder(GetHandle(), folder);
        if (hr == S_OK) {
          result->Success(flutter::EncodableValue(folder));
        } else if (hr == S_FALSE) {
          result->Success();
        } else {
          result->Error("PICK_FAILED");
        }
      });

  SetChildContent(flutter_controller_->view()->GetNativeWindow());

  flutter_controller_->engine()->SetNextFrameCallback([&]() {
    this->Show();
  });

  // Flutter can complete the first frame before the "show window" callback is
  // registered. The following call ensures a frame is pending to ensure the
  // window is shown. It is a no-op if the first frame hasn't completed yet.
  flutter_controller_->ForceRedraw();

  return true;
}

void FlutterWindow::OnDestroy() {
  // Remove the channel before the engine goes away.
  folder_picker_channel_ = nullptr;
  if (flutter_controller_) {
    flutter_controller_ = nullptr;
  }

  Win32Window::OnDestroy();
}

LRESULT
FlutterWindow::MessageHandler(HWND hwnd, UINT const message,
                              WPARAM const wparam,
                              LPARAM const lparam) noexcept {
  // Give Flutter, including plugins, an opportunity to handle window messages.
  if (flutter_controller_) {
    std::optional<LRESULT> result =
        flutter_controller_->HandleTopLevelWindowProc(hwnd, message, wparam,
                                                      lparam);
    if (result) {
      return *result;
    }
  }

  switch (message) {
    case WM_FONTCHANGE:
      flutter_controller_->engine()->ReloadSystemFonts();
      break;
  }

  return Win32Window::MessageHandler(hwnd, message, wparam, lparam);
}
