#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <filesystem>
#include <functional>
#include <iostream>
#include <map>
#include <memory>
#include <set>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include <unistd.h>

#include "include/base/cef_callback.h"
#include "include/base/cef_compiler_specific.h"
#include "include/cef_app.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/cef_command_line.h"
#include "include/cef_dialog_handler.h"
#include "include/cef_download_handler.h"
#include "include/cef_find_handler.h"
#include "include/cef_focus_handler.h"
#include "include/cef_keyboard_handler.h"
#include "include/cef_frame.h"
#include "include/cef_parser.h"
#include "include/cef_permission_handler.h"
#include "include/cef_request.h"
#include "include/cef_request_context.h"
#include "include/cef_request_context_handler.h"
#include "include/cef_resource_handler.h"
#include "include/cef_resource_request_handler.h"
#include "include/cef_response.h"
#include "include/cef_values.h"
#include "include/views/cef_box_layout.h"
#include "include/views/cef_browser_view.h"
#include "include/views/cef_fill_layout.h"
#include "include/views/cef_overlay_controller.h"
#include "include/views/cef_window.h"
#include "include/wrapper/cef_closure_task.h"
#include "browser_features.h"
#include "session.h"
#include "startup.h"

#ifndef CEF_USE_SANDBOX
#error OpenShell requires a sandbox-enabled CEF build
#endif

namespace {

using namespace openshell;

constexpr char kTitle[] = "OpenShell Native";
// Requests of the Desktop page that the host answers itself. They never reach hostd.
constexpr char kShellPath[] = "/_shell/";
constexpr int kBridgeVersion = 1;
constexpr std::size_t kMaxCommandBytes = 64 * 1024;
constexpr std::size_t kMaxTabs = 64;
constexpr int kMaxViewSize = 16384;
constexpr std::size_t kMaxRendererRestarts = 3;
constexpr auto kRendererRestartWindow = std::chrono::minutes(1);

bool ReportsDesktopHealth() {
  const char* enabled = std::getenv("OPENSHELL_NATIVE_HEALTH");
  return enabled && std::strcmp(enabled, "1") == 0;
}

void DesktopHealth(bool healthy) {
  if (ReportsDesktopHealth()) {
    std::cout << (healthy ? "OPENSHELL_DESKTOP_HEALTHY" : "OPENSHELL_DESKTOP_HEALTH_RESET") << std::endl;
  }
}

std::uint32_t ContextOf(Kind kind) {
  switch (kind) {
    case Kind::kPrimary:
      return OPEN_SHELL_PRIMARY;
    case Kind::kGuest:
      return OPEN_SHELL_GUEST;
    case Kind::kBrowser:
      return OPEN_SHELL_BROWSER;
  }
  return OPEN_SHELL_GUEST;
}

const char* NameOf(Kind kind) {
  switch (kind) {
    case Kind::kPrimary:
      return "desktop";
    case Kind::kGuest:
      return "guest";
    case Kind::kBrowser:
      return "browser";
  }
  return "unknown";
}

bool Allows(const Startup& startup, Kind kind, OpenshellCefOperation operation,
            const CefString& url) {
  const auto value = url.ToString();
  return openshell_cef_allows(startup.get(), ContextOf(kind), operation,
                              value.data(), value.size());
}

constexpr int kShortcutModifiers =
    EVENTFLAG_ALT_DOWN | EVENTFLAG_SHIFT_DOWN | EVENTFLAG_CONTROL_DOWN | EVENTFLAG_COMMAND_DOWN;

void CloseTab(Session& session, const std::string& id);
void CloseTabs(Session& session);

void CloseSession(std::shared_ptr<Session> session) {
  session->closing = true;
  session->clipboard.reset();
  CloseTabs(*session);
  if (session->desktop) {
    session->desktop->GetHost()->CloseBrowser(true);
  } else if (session->browsers == 0) {
    CefQuitMessageLoop();
  }
}

// The launcher restarted hostd: start over from its new bootstrap URL, which
// trades the new secret for the session cookie.
void LoadBootstrap(std::shared_ptr<Session> session, std::string url) {
  if (session->closing) {
    return;
  }
  if (!session->desktop) {
    session->pending_bootstrap = std::move(url);
    return;
  }
  session->renderer_restarts.clear();
  session->desktop->GetMainFrame()->LoadURL(url);
}

void ReloadDesktop(std::shared_ptr<Session> session) {
  if (session->desktop && !session->closing) {
    session->desktop->Reload();
  }
}

// Reads the launcher's messages until the launcher closes the pipe. The
// session ends with the launcher, or when it sends anything unexpected.
void ReadControl(OpenshellCefControl* control, std::shared_ptr<Session> session) {
  std::vector<char> buffer(4096);
  while (openshell_cef_control_next(control, buffer.data(), buffer.size()) >= 0) {
    CefPostTask(TID_UI, base::BindOnce(&LoadBootstrap, session,
                                       std::string(buffer.data())));
  }
  openshell_cef_control_free(control);
  CefPostTask(TID_UI, base::BindOnce(&CloseSession, session));
}

// The DOM `key` and `code` of a key press, for the keys that shortcuts use.
// Letters take the layout's letter, as the DOM `key` does.
bool DomKey(const CefKeyEvent& event, std::string& key, std::string& code) {
  const int vk = event.windows_key_code;
  if (vk >= 'A' && vk <= 'Z') {
    key = std::string(1, static_cast<char>(vk - 'A' + 'a'));
    code = std::string("Key") + static_cast<char>(vk);
    return true;
  }
  if (vk >= '0' && vk <= '9') {
    key = std::string(1, static_cast<char>(vk));
    code = std::string("Digit") + static_cast<char>(vk);
    return true;
  }
  if (vk >= 0x70 && vk <= 0x87) {
    key = code = "F" + std::to_string(vk - 0x6f);
    return true;
  }
  static const struct {
    int vk;
    const char* key;
    const char* code;
  } kKeys[] = {
      {0x08, "Backspace", "Backspace"}, {0x09, "Tab", "Tab"},
      {0x0d, "Enter", "Enter"},         {0x1b, "Escape", "Escape"},
      {0x12, "Alt", "AltLeft"},        {0xa4, "Alt", "AltLeft"},
      {0xa5, "Alt", "AltRight"},
      {0x20, " ", "Space"},             {0x21, "PageUp", "PageUp"},
      {0x22, "PageDown", "PageDown"},   {0x23, "End", "End"},
      {0x24, "Home", "Home"},           {0x25, "ArrowLeft", "ArrowLeft"},
      {0x26, "ArrowUp", "ArrowUp"},     {0x27, "ArrowRight", "ArrowRight"},
      {0x28, "ArrowDown", "ArrowDown"}, {0x2d, "Insert", "Insert"},
      {0x2e, "Delete", "Delete"},       {0x5b, "Meta", "MetaLeft"},
      {0x5c, "Meta", "MetaRight"},      {0xba, ";", "Semicolon"},
      {0xbb, "=", "Equal"},             {0xbc, ",", "Comma"},
      {0xbd, "-", "Minus"},             {0xbe, ".", "Period"},
      {0xbf, "/", "Slash"},             {0xc0, "`", "Backquote"},
      {0xdb, "[", "BracketLeft"},       {0xdc, "\\", "Backslash"},
      {0xdd, "]", "BracketRight"},      {0xde, "'", "Quote"},
      // The computer's keys (shared/hardwareKeys.ts in the Desktop).
      {0xad, "AudioVolumeMute", "AudioVolumeMute"},
      {0xae, "AudioVolumeDown", "AudioVolumeDown"},
      {0xaf, "AudioVolumeUp", "AudioVolumeUp"},
      {0xd8, "BrightnessDown", "BrightnessDown"},
      {0xd9, "BrightnessUp", "BrightnessUp"},
  };
  for (const auto& entry : kKeys) {
    if (entry.vk == vk) {
      key = entry.key;
      code = entry.code;
      return true;
    }
  }
  return false;
}

std::string Lowercase(std::string text) {
  std::transform(text.begin(), text.end(), text.begin(),
                 [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
  return text;
}

void FocusDesktop(Session& session) {
  for (auto& [id, tab] : session.tabs) {
    tab.focus_on_show = false;
  }
  if (session.desktop) {
    if (auto view = CefBrowserView::GetForBrowser(session.desktop)) {
      if (session.window) {
        session.window->Activate();
      }
      view->RequestFocus();
      session.desktop->GetHost()->SetFocus(true);
    }
  }
}

bool TabIsShown(const Tab& tab) {
  return tab.visible && !tab.closing && tab.bounds.width > 0 && tab.bounds.height > 0;
}

void ApplyBounds(Session& session, Tab& tab) {
  if (!tab.overlay) {
    return;
  }
  const bool shown = TabIsShown(tab);
  if (shown) {
    tab.overlay->SetBounds(tab.bounds);
  }
  if (tab.overlay->IsVisible() != shown) {
    const bool return_focus = !shown && tab.view->HasFocus();
    tab.overlay->SetVisible(shown);
    if (return_focus && session.desktop) {
      // Covered for a moment (the volume display), the tab takes the keyboard
      // back when shown, unless the person uses the Desktop meanwhile
      // (FocusDesktop forgets it).
      tab.focus_on_show = !tab.closing;
      if (auto desktop = CefBrowserView::GetForBrowser(session.desktop)) {
        desktop->RequestFocus();
        session.desktop->GetHost()->SetFocus(true);
      }
    }
  }
  if (shown && tab.focus_on_show && session.window->IsActive()) {
    tab.focus_on_show = false;
    tab.view->RequestFocus();
    if (tab.browser) {
      tab.browser->GetHost()->SetFocus(true);
    }
  }
}

class ViewDelegate final : public CefBrowserViewDelegate {
 public:
  ViewDelegate() = default;
  // A tab's view: its DevTools are docked in the Desktop's window.
  ViewDelegate(std::weak_ptr<Session> session, std::string tab)
      : session_(std::move(session)), tab_(std::move(tab)) {}
  explicit ViewDelegate(cef_runtime_style_t style) : style_(style) {}

  cef_runtime_style_t GetBrowserRuntimeStyle() override {
    return style_;
  }

  CefRefPtr<CefBrowserViewDelegate> GetDelegateForPopupBrowserView(
      CefRefPtr<CefBrowserView>, const CefBrowserSettings&, CefRefPtr<CefClient>,
      bool is_devtools) override {
    return is_devtools ? new ViewDelegate(CEF_RUNTIME_STYLE_CHROME) : nullptr;
  }

  bool OnPopupBrowserViewCreated(CefRefPtr<CefBrowserView>, CefRefPtr<CefBrowserView> popup,
                                 bool is_devtools) override {
    auto session = session_.lock();
    return is_devtools && session && !tab_.empty() &&
           features::DevToolsCreated(*session, tab_, popup);
  }

 private:
  const std::weak_ptr<Session> session_;
  const std::string tab_;
  const cef_runtime_style_t style_ = CEF_RUNTIME_STYLE_ALLOY;
  IMPLEMENT_REFCOUNTING(ViewDelegate);
};

// The text without the query or fragment of any address in it, at most `limit` bytes.
std::string WithoutQueries(const std::string& text, std::size_t limit) {
  std::string result;
  bool skipping = false;
  for (const char character : text) {
    if (result.size() >= limit) {
      result += "...";
      break;
    }
    if (skipping) {
      if (character != ' ' && character != '"' && character != '\'' && character != ')' && character != '\n') {
        continue;
      }
      skipping = false;
    } else if ((character == '?' || character == '#') && !result.empty() && result.back() != ' ') {
      skipping = true;
      result += "?...";
      continue;
    }
    result += character;
  }
  return result;
}

class BrowserClient final : public CefClient,
                            public CefDialogHandler,
                            public CefDisplayHandler,
                            public CefDownloadHandler,
                            public CefFindHandler,
                            public CefFocusHandler,
                            public CefKeyboardHandler,
                            public CefLifeSpanHandler,
                            public CefLoadHandler,
                            public CefPermissionHandler,
                            public CefRequestHandler {
 public:
  BrowserClient(Startup startup, Kind kind, bool desktop,
                std::shared_ptr<Session> session, std::string tab = {})
      : startup_(std::move(startup)), kind_(kind), desktop_(desktop),
        session_(std::move(session)), tab_(std::move(tab)) {}

  CefRefPtr<CefDialogHandler> GetDialogHandler() override { return this; }
  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }
  CefRefPtr<CefDownloadHandler> GetDownloadHandler() override { return this; }
  CefRefPtr<CefFindHandler> GetFindHandler() override { return this; }
  CefRefPtr<CefFocusHandler> GetFocusHandler() override { return this; }
  CefRefPtr<CefKeyboardHandler> GetKeyboardHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefPermissionHandler> GetPermissionHandler() override { return this; }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }

  bool OnBeforeBrowse(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                      CefRefPtr<CefRequest> request, bool, bool) override {
    const bool main = !frame || frame->IsMain();
    const auto url = request->GetURL();
    if (Allows(startup_, kind_, main ? OPEN_SHELL_MAIN_FRAME : OPEN_SHELL_SUBFRAME, url)) {
      if (IsDesktop() && main) {
        DesktopHealth(false);
      }
      return false;
    }
    if (IsTab() && main) {
      auto event = TabEvent(tab_, "blocked");
      event->SetString("url", url);
      Emit(*session_, event);
    }
    return true;
  }

  // Links that ask for a new tab or window open a Browser tab, chosen by the
  // Desktop. Every other popup is refused.
  bool OnOpenURLFromTab(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>,
                        const CefString& url, cef_window_open_disposition_t,
                        bool) override {
    RequestTab(url);
    return true;
  }

  bool OnBeforePopup(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, int,
                     const CefString& url, const CefString&,
                     cef_window_open_disposition_t, bool, const CefPopupFeatures&,
                     CefWindowInfo&, CefRefPtr<CefClient>&, CefBrowserSettings&,
                     CefRefPtr<CefDictionaryValue>&, bool*) override {
    RequestTab(url);
    return true;
  }

  // Route the real press and release so the Desktop can keep its switcher
  // open while Alt is held.
  bool OnPreKeyEvent(CefRefPtr<CefBrowser> browser, const CefKeyEvent& event, CefEventHandle,
                     bool*) override {
    if (!IsTab() || (event.modifiers & EVENTFLAG_ALTGR_DOWN) ||
        (event.type != KEYEVENT_RAWKEYDOWN && event.type != KEYEVENT_KEYUP)) {
      return false;
    }
    if (event.type == KEYEVENT_RAWKEYDOWN) {
      const auto tab = session_->tabs.find(tab_);
      if (tab != session_->tabs.end() && tab->second.fullscreen) {
        // As on Live, Escape and switching windows end a page's fullscreen;
        // the switch still goes to the Desktop.
        const bool escape = event.windows_key_code == 0x1b && !(event.modifiers & kShortcutModifiers);
        const bool switching = (event.modifiers & EVENTFLAG_ALT_DOWN) &&
                               (event.windows_key_code == 0x09 || event.windows_key_code == 0xc0);
        if (escape || switching) {
          browser->GetMainFrame()->ExecuteJavaScript(
              "if (document.fullscreenElement) document.exitFullscreen().catch(() => {});", "", 0);
          if (escape) {
            return true;
          }
        }
      }
    }
    std::string key;
    std::string code;
    if (!DomKey(event, key, code)) {
      return false;
    }
    const auto lower = Lowercase(key);
    const int modifiers = event.modifiers & kShortcutModifiers;
    const bool pressed = event.type == KEYEVENT_RAWKEYDOWN;
    bool move_focus = false;
    if (!pressed && !session_->shortcut_phases) {
      return false;
    }
    if (pressed) {
      const auto shortcut = std::find_if(
          session_->shortcuts.begin(), session_->shortcuts.end(), [&](const Shortcut& entry) {
            return entry.key == lower && entry.modifiers == modifiers;
          });
      if (shortcut == session_->shortcuts.end()) {
        routed_keys_.erase(event.windows_key_code);
        return false;
      }
      if (session_->shortcut_phases) {
        routed_keys_.insert(event.windows_key_code);
      }
      move_focus = shortcut->focus_desktop;
    } else if (routed_keys_.erase(event.windows_key_code) == 0) {
      // Alt or Super may have been pressed before this tab got the focus.
      if ((key != "Alt" && key != "Meta") || session_->shortcuts.empty()) {
        return false;
      }
    }
    auto stroke = TabEvent(tab_, "key");
    stroke->SetString("key", key);
    stroke->SetString("code", code);
    stroke->SetString("phase", pressed ? "keydown" : "keyup");
    stroke->SetBool("repeat", event.modifiers & EVENTFLAG_IS_REPEAT);
    stroke->SetBool("alt", event.modifiers & EVENTFLAG_ALT_DOWN);
    stroke->SetBool("shift", event.modifiers & EVENTFLAG_SHIFT_DOWN);
    stroke->SetBool("ctrl", event.modifiers & EVENTFLAG_CONTROL_DOWN);
    stroke->SetBool("meta", event.modifiers & EVENTFLAG_COMMAND_DOWN);
    if (move_focus) {
      // Let Chromium finish handling the consumed key before moving focus
      // from the overlay widget to the Desktop's top-level widget.
      CefPostTask(TID_UI, base::BindOnce(
                              [](std::shared_ptr<Session> session, CefRefPtr<CefBrowser> source,
                                 CefRefPtr<CefDictionaryValue> stroke) {
                                if (session->closing || !source->IsValid()) {
                                  return;
                                }
                                source->GetHost()->SetFocus(false);
                                FocusDesktop(*session);
                                Emit(*session, stroke);
                              },
                              session_, browser, stroke));
    } else {
      Emit(*session_, stroke);
    }
    return true;
  }

  // A key the page left unhandled goes on to the window, which must not
  // deliver it again.
  bool OnKeyEvent(CefRefPtr<CefBrowser>, const CefKeyEvent& event, CefEventHandle) override {
    auto& seen = session_->seen_keys;
    seen.push_back({event.type, event.windows_key_code, std::chrono::steady_clock::now()});
    if (seen.size() > 32) {
      seen.pop_front();
    }
    return false;
  }

  void OnGotFocus(CefRefPtr<CefBrowser>) override {
    if (!IsTab()) {
      return;
    }
    const auto tab = session_->tabs.find(tab_);
    if (tab != session_->tabs.end() && TabIsShown(tab->second)) {
      Emit(*session_, TabEvent(tab_, "focused"));
    }
  }

  void OnFindResult(CefRefPtr<CefBrowser>, int, int count, const CefRect&, int active,
                    bool final_update) override {
    if (IsTab()) {
      auto event = TabEvent(tab_, "found");
      event->SetInt("total", count);
      event->SetInt("active", active);
      event->SetBool("final", final_update);
      Emit(*session_, event);
    }
  }

  // Only the Browser's tabs download, into the Downloads folder of the
  // session's home, which Files shows.
  bool CanDownload(CefRefPtr<CefBrowser>, const CefString&, const CefString&) override {
    return IsTab();
  }

  bool OnBeforeDownload(CefRefPtr<CefBrowser> browser, CefRefPtr<CefDownloadItem> item,
                        const CefString& suggested_name,
                        CefRefPtr<CefBeforeDownloadCallback> callback) override {
    if (!IsTab() || !features::BeforeDownload(*session_, tab_, item, suggested_name, callback)) {
      return false;
    }
    // Starting a download takes the keyboard from the page; it goes back.
    CefPostTask(TID_UI, base::BindOnce(
                            [](CefRefPtr<CefBrowser> page) {
                              if (auto view = CefBrowserView::GetForBrowser(page)) {
                                view->RequestFocus();
                              }
                              page->GetHost()->SetFocus(true);
                            },
                            browser));
    return true;
  }

  void OnDownloadUpdated(CefRefPtr<CefBrowser>, CefRefPtr<CefDownloadItem> item,
                         CefRefPtr<CefDownloadItemCallback> callback) override {
    if (IsTab()) {
      features::DownloadUpdated(*session_, item, callback);
    }
  }

  bool OnRequestMediaAccessPermission(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>,
                                      const CefString& origin, uint32_t permissions,
                                      CefRefPtr<CefMediaAccessCallback> callback) override {
    if (IsDesktop()) {
      // As on Live, the shell's own page has the microphone, the camera and
      // the screen without asking: its screen recordings and its Settings.
      callback->Continue(permissions);
      return true;
    }
    return IsTab() && features::MediaAccess(*session_, tab_, origin, permissions, callback);
  }

  bool OnShowPermissionPrompt(CefRefPtr<CefBrowser>, uint64_t, const CefString&,
                              uint32_t permissions,
                              CefRefPtr<CefPermissionPromptCallback> callback) override {
    return IsTab() && features::PermissionPrompt(*session_, tab_, permissions, callback);
  }

  // Uploads and the Desktop's own "choose a file" open the Desktop's picker.
  bool OnFileDialog(CefRefPtr<CefBrowser> browser, FileDialogMode mode, const CefString&,
                    const CefString&, const std::vector<CefString>& accept,
                    const std::vector<CefString>&, const std::vector<CefString>&,
                    CefRefPtr<CefFileDialogCallback> callback) override {
    if (!IsTab() && !IsDesktop()) {
      return false;
    }
    const CefString page = IsTab() ? browser->GetMainFrame()->GetURL() : CefString();
    return features::FileDialog(*session_, page, mode, accept, callback);
  }

  bool OnCertificateError(CefRefPtr<CefBrowser>, cef_errorcode_t, const CefString& url,
                          CefRefPtr<CefSSLInfo> ssl, CefRefPtr<CefCallback> callback) override {
    return IsTab() && features::CertificateError(*session_, url, ssl, callback);
  }

  // A tab's page entered or left HTML fullscreen. With Alloy views the client
  // sizes the view: the Desktop lays the tab over the whole screen meanwhile.
  void OnFullscreenModeChange(CefRefPtr<CefBrowser>, bool fullscreen) override {
    if (IsDesktop()) {
      // An element of the Desktop's page (a video, an application's frame) covers the screen.
      std::cerr << "desktop page fullscreen: " << (fullscreen ? "on" : "off") << std::endl;
    }
    if (!IsTab()) {
      return;
    }
    const auto tab = session_->tabs.find(tab_);
    if (tab == session_->tabs.end() || tab->second.closing || tab->second.fullscreen == fullscreen) {
      return;
    }
    tab->second.fullscreen = fullscreen;
    auto event = TabEvent(tab_, "fullscreen");
    event->SetBool("fullscreen", fullscreen);
    Emit(*session_, event);
  }

  // The Desktop page's errors go to the session log: when the UI does not
  // come up, they say why. Addresses lose their query and fragment, where the
  // bootstrap secret would be.
  bool OnConsoleMessage(CefRefPtr<CefBrowser>, cef_log_severity_t level, const CefString& message,
                        const CefString& source, int line) override {
    if (IsDesktop() && level >= LOGSEVERITY_ERROR) {
      std::cerr << "desktop console: " << WithoutQueries(message.ToString(), 500) << " ("
                << WithoutQueries(source.ToString(), 200) << ":" << line << ")" << std::endl;
    }
    return false;
  }

  void OnAddressChange(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                       const CefString& url) override {
    if (IsTab() && frame && frame->IsMain()) {
      auto event = TabEvent(tab_, "address");
      event->SetString("url", url);
      Emit(*session_, event);
    }
  }

  void OnLoadingStateChange(CefRefPtr<CefBrowser>, bool loading, bool back,
                            bool forward) override {
    if (IsTab()) {
      auto event = TabEvent(tab_, "state");
      event->SetBool("loading", loading);
      event->SetBool("back", back);
      event->SetBool("forward", forward);
      Emit(*session_, event);
    }
  }

  // A reload after a renderer or hostd restart leaves the view without the
  // keyboard focus. The Desktop owns the window, so it takes the focus back.
  void OnLoadEnd(CefRefPtr<CefBrowser> browser, CefRefPtr<CefFrame> frame,
                 int) override {
    if (!frame || !frame->IsMain() || !(IsDesktop() || IsTab())) {
      return;
    }
    auto view = CefBrowserView::GetForBrowser(browser);
    // A tab focused before its renderer existed (a popup, say) holds the
    // view's focus, but its page never got the keyboard; the same goes for a
    // new renderer of the Desktop.
    if (IsTab() && !(view && view->HasFocus())) {
      return;
    }
    auto window = view ? view->GetWindow() : nullptr;
    if (window && window->IsActive()) {
      // Activating the window again tells the focused renderer.
      window->Deactivate();
      window->Activate();
    }
    if (view) {
      view->RequestFocus();
      browser->GetHost()->SetFocus(true);
    }
  }

  void OnLoadError(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                   ErrorCode error, const CefString&,
                   const CefString& url) override {
    if (!frame || !frame->IsMain() || error == ERR_ABORTED) {
      return;
    }
    if (IsTab()) {
      auto event = TabEvent(tab_, "error");
      event->SetInt("code", error);
      event->SetString("url", url);
      Emit(*session_, event);
    } else {
      std::cerr << "CEF " << NameOf(kind_) << " navigation failed (" << error << ")\n";
    }
  }

  void OnRenderProcessTerminated(CefRefPtr<CefBrowser> browser,
                                 TerminationStatus status, int error_code,
                                 const CefString& error_string) override {
    std::cerr << "CEF " << NameOf(kind_) << " renderer terminated (status "
              << status << ", code " << error_code << ", "
              << error_string.ToString() << ")\n";
    if (IsTab()) {
      Emit(*session_, TabEvent(tab_, "crashed"));
      return;
    }
    if (!IsDesktop() || session_->closing) {
      return;
    }
    DesktopHealth(false);
    CloseTabs(*session_);
    // The Desktop reloads with its session cookie, a few times per minute.
    auto& restarts = session_->renderer_restarts;
    const auto now = std::chrono::steady_clock::now();
    while (!restarts.empty() && now - restarts.front() > kRendererRestartWindow) {
      restarts.pop_front();
    }
    if (restarts.size() >= kMaxRendererRestarts) {
      std::cerr << "CEF desktop renderer keeps failing; ending the session\n";
      session_->failed = true;
      CloseSession(session_);
      return;
    }
    restarts.push_back(now);
    // Reload once Chromium has finished handling the exit.
    CefPostTask(TID_UI, base::BindOnce(&ReloadDesktop, session_));
  }

  void OnTitleChange(CefRefPtr<CefBrowser> browser,
                     const CefString& title) override {
    if (IsTab()) {
      auto event = TabEvent(tab_, "title");
      event->SetString("title", title);
      Emit(*session_, event);
    } else if (kind_ == Kind::kPrimary && !desktop_) {
      // The probes find their window by the fixture's title.
      if (auto view = CefBrowserView::GetForBrowser(browser)) {
        if (auto window = view->GetWindow()) {
          window->SetTitle(title);
        }
      }
    }
  }

  void OnAfterCreated(CefRefPtr<CefBrowser> browser) override {
    ++session_->browsers;
    if (IsTab()) {
      auto tab = session_->tabs.find(tab_);
      if (tab == session_->tabs.end() || tab->second.closing) {
        browser->GetHost()->CloseBrowser(true);
      } else {
        tab->second.browser = browser;
        features::TabCreated(*session_, tab_);
      }
      return;
    }
    if (!IsDesktop()) {
      return;
    }
    session_->desktop = browser;
    if (desktop_ && !session_->clipboard) {
      // Copies of any program, for the Desktop's clipboard history.
      session_->clipboard = ClipboardWatcher::Start(
          [weak = std::weak_ptr<Session>(session_)](ClipboardCopy copy) {
            CefPostTask(TID_UI, base::BindOnce(
                                    [](std::weak_ptr<Session> weak, ClipboardCopy copy) {
                                      if (auto session = weak.lock(); session && !session->closing) {
                                        features::ClipboardCopied(*session, copy);
                                      }
                                    },
                                    weak, std::move(copy)));
          });
    }
    session_->desktop_id = browser->GetIdentifier();
    if (session_->closing) {
      CloseSession(session_);
    } else if (!session_->pending_bootstrap.empty()) {
      browser->GetMainFrame()->LoadURL(
          std::exchange(session_->pending_bootstrap, std::string()));
    }
  }

  void OnBeforeClose(CefRefPtr<CefBrowser> browser) override {
    if (IsTab()) {
      auto tab = session_->tabs.find(tab_);
      if (tab != session_->tabs.end() && tab->second.browser &&
          tab->second.browser->IsSame(browser)) {
        features::TabClosing(*session_, tab_);
        if (tab->second.overlay) {
          tab->second.overlay->Destroy();
        }
        session_->tabs.erase(tab);
        Emit(*session_, TabEvent(tab_, "closed"));
      }
    } else if (session_->desktop && session_->desktop->IsSame(browser)) {
      session_->desktop = nullptr;
      session_->desktop_id = 0;
    }
    if (--session_->browsers == 0) {
      CefQuitMessageLoop();
    }
  }

 private:
  bool IsDesktop() const { return kind_ == Kind::kPrimary && desktop_; }
  bool IsTab() const { return kind_ == Kind::kBrowser; }

  void RequestTab(const CefString& url) {
    if (IsTab() && Allows(startup_, kind_, OPEN_SHELL_MAIN_FRAME, url)) {
      auto event = TabEvent(tab_, "popup");
      event->SetString("url", url);
      Emit(*session_, event);
    }
  }

  const Startup startup_;
  const Kind kind_;
  const bool desktop_;
  const std::shared_ptr<Session> session_;
  const std::string tab_;
  std::set<int> routed_keys_;
  IMPLEMENT_REFCOUNTING(BrowserClient);
};

void CreateTabView(Session& session, const std::string& id) {
  auto entry = session.tabs.find(id);
  if (entry == session.tabs.end() || entry->second.view || entry->second.closing) {
    return;
  }
  Tab& tab = entry->second;
  auto context = tab.incognito ? session.private_context : session.browser_context;
  tab.view = CefBrowserView::CreateBrowserView(
      new BrowserClient(session.startup, Kind::kBrowser, true,
                        session.shared_from_this(), id),
      tab.url, CefBrowserSettings(), nullptr, context,
      new ViewDelegate(session.weak_from_this(), id));
  if (tab.view && session.window) {
    tab.overlay = session.window->AddOverlayView(tab.view, CEF_DOCKING_MODE_CUSTOM, true);
  }
  if (!tab.overlay) {
    session.tabs.erase(entry);
    Emit(session, TabEvent(id, "closed"));
    return;
  }
  tab.overlay->SetVisible(false);
  ApplyBounds(session, tab);
}

class RequestContextHandler;

// Creates the Browser's normal or private context. A context with an on-disk
// profile initializes asynchronously; its tabs wait for it.
void EnsureBrowserContext(Session& session, bool incognito);

void CloseTab(Session& session, const std::string& id) {
  auto entry = session.tabs.find(id);
  if (entry == session.tabs.end()) {
    return;
  }
  Tab& tab = entry->second;
  tab.closing = true;
  ApplyBounds(session, tab);
  if (tab.browser) {
    // OnBeforeClose removes the view and the tab.
    tab.browser->GetHost()->CloseBrowser(true);
    return;
  }
  if (tab.overlay) {
    tab.overlay->Destroy();
  }
  session.tabs.erase(entry);
  Emit(session, TabEvent(id, "closed"));
}

bool ValidTabId(const std::string& id) {
  return !id.empty() && id.size() <= 64 &&
         std::all_of(id.begin(), id.end(), [](unsigned char c) {
           return std::isalnum(c) || c == '-' || c == '_';
         });
}

bool BrowserUrl(const Session& session, const std::string& url) {
  return url.size() < 8192 &&
         openshell_cef_allows(session.startup.get(), OPEN_SHELL_BROWSER,
                              OPEN_SHELL_MAIN_FRAME, url.data(), url.size());
}

int Coordinate(CefRefPtr<CefDictionaryValue> args, const char* key, int low) {
  if (args->GetType(key) == VTYPE_INT) {
    return std::clamp(args->GetInt(key), low, kMaxViewSize);
  }
  if (args->GetType(key) == VTYPE_DOUBLE) {
    const double value = args->GetDouble(key);
    return value != value ? 0 : static_cast<int>(std::clamp<double>(value, low, kMaxViewSize));
  }
  return 0;
}

CefRect PlacementBounds(CefRefPtr<CefDictionaryValue> placement) {
  return CefRect(Coordinate(placement, "x", -kMaxViewSize),
                 Coordinate(placement, "y", -kMaxViewSize),
                 Coordinate(placement, "width", 0), Coordinate(placement, "height", 0));
}

Reply ApplyLayout(Session& session, CefRefPtr<CefDictionaryValue> args) {
  const int generation = args->GetType("generation") == VTYPE_INT ? args->GetInt("generation") : -1;
  auto surfaces = args->GetList("surfaces");
  if (generation < 0 || !surfaces || surfaces->GetSize() > kMaxTabs) {
    return Error(400, "The layout needs a generation and a list of surfaces");
  }
  if (generation <= session.layout_generation) {
    return {};
  }

  std::map<std::string, CefRefPtr<CefDictionaryValue>> placements;
  const auto focus = args->GetString("focus").ToString();
  for (std::size_t index = 0; index < surfaces->GetSize(); ++index) {
    auto placement = surfaces->GetDictionary(index);
    const auto id = placement ? placement->GetString("id").ToString() : std::string();
    if (!ValidTabId(id) || placement->GetType("visible") != VTYPE_BOOL ||
        !placements.emplace(id, placement).second) {
      return Error(400, "Each surface needs a unique tab id and visibility");
    }
    for (const char* coordinate : {"x", "y", "width", "height"}) {
      const auto type = placement->GetType(coordinate);
      if (type != VTYPE_INT && type != VTYPE_DOUBLE) {
        return Error(400, "Each surface needs numeric bounds");
      }
    }
  }
  if (!focus.empty() && (!placements.count(focus) || !placements.at(focus)->GetBool("visible"))) {
    return Error(400, "The focus target must be visible");
  }

  session.layout_generation = generation;
  for (auto& [id, tab] : session.tabs) {
    const auto placement = placements.find(id);
    tab.visible = placement != placements.end() && placement->second->GetBool("visible");
    if (placement != placements.end()) {
      tab.bounds = PlacementBounds(placement->second);
    }
    tab.focus_on_show = TabIsShown(tab) && (id == focus || (focus.empty() && tab.focus_on_show));
  }
  // Hide outgoing surfaces before showing incoming surfaces in this UI task.
  for (auto& [id, tab] : session.tabs) {
    if (!TabIsShown(tab)) {
      ApplyBounds(session, tab);
    }
  }
  for (auto& [id, tab] : session.tabs) {
    if (TabIsShown(tab)) {
      ApplyBounds(session, tab);
    }
  }
  return {};
}

void CloseTabs(Session& session) {
  std::vector<std::string> ids;
  for (const auto& [id, tab] : session.tabs) {
    ids.push_back(id);
  }
  for (const auto& id : ids) {
    CloseTab(session, id);
  }
}

Reply RunCommand(Session& session, const std::string& command, const std::string& body) {
  if (command == "hello") {
    // A new Desktop page: the tabs of the previous one have no window anymore.
    CloseTabs(session);
    features::DesktopReset(session);
    session.layout_generation = -1;
    session.shortcuts.clear();
    session.shortcut_phases = false;
    auto value = CefValue::Create();
    auto hello = CefDictionaryValue::Create();
    hello->SetInt("version", kBridgeVersion);
    hello->SetBool("browser", true);
    hello->SetBool("surfaceLayout", true);
    hello->SetBool("shortcutPhases", true);
    hello->SetBool("overlays", true);
    hello->SetBool("health", ReportsDesktopHealth());
    hello->SetString("event", kBrowserEvent);
    value->SetDictionary(hello);
    return {200, CefWriteJSON(value, JSON_WRITER_DEFAULT).ToString()};
  }
  auto parsed = CefParseJSON(body.data(), body.size(), JSON_PARSER_RFC);
  if (!parsed || parsed->GetType() != VTYPE_DICTIONARY) {
    return Error(400, "The command takes a JSON object");
  }
  auto args = parsed->GetDictionary();
  if (command == "desktop/health") {
    DesktopHealth(args->GetBool("healthy"));
    return {};
  }
  if (command == "focus-desktop") {
    FocusDesktop(session);
    return {};
  }
  if (command == "browser/layout") {
    return ApplyLayout(session, args);
  }
  if (command == "browser/shortcuts") {
    if (args->GetType("shortcuts") != VTYPE_LIST) {
      return Error(400, "The shortcuts are a list");
    }
    auto list = args->GetList("shortcuts");
    if (list->GetSize() > 1024) {
      return Error(400, "Too many shortcuts");
    }
    std::vector<Shortcut> shortcuts;
    static const struct {
      const char* name;
      int flag;
    } kModifiers[] = {{"alt", EVENTFLAG_ALT_DOWN},
                      {"shift", EVENTFLAG_SHIFT_DOWN},
                      {"ctrl", EVENTFLAG_CONTROL_DOWN},
                      {"meta", EVENTFLAG_COMMAND_DOWN}};
    for (std::size_t index = 0; index < list->GetSize(); ++index) {
      auto entry = list->GetDictionary(index);
      if (!entry || entry->GetString("key").ToString().empty()) {
        return Error(400, "A shortcut names a key");
      }
      Shortcut shortcut;
      shortcut.key = Lowercase(entry->GetString("key").ToString().substr(0, 32));
      shortcut.focus_desktop = entry->GetBool("focusDesktop");
      for (const auto& modifier : kModifiers) {
        if (entry->GetBool(modifier.name)) {
          shortcut.modifiers |= modifier.flag;
        }
      }
      shortcuts.push_back(std::move(shortcut));
    }
    session.shortcuts = std::move(shortcuts);
    session.shortcut_phases = args->GetBool("keyPhases");
    return {};
  }
  if (command == "browser/end-private") {
    std::vector<std::string> ids;
    for (const auto& [id, tab] : session.tabs) {
      if (tab.incognito) {
        ids.push_back(id);
      }
    }
    for (const auto& id : ids) {
      CloseTab(session, id);
    }
    // The next private tab starts with an empty context.
    session.private_context = nullptr;
    session.private_ready = false;
    return {};
  }
  const auto id = args->GetString("id").ToString();
  if (!ValidTabId(id)) {
    return Error(400, "Invalid tab id");
  }
  auto entry = session.tabs.find(id);
  if (command == "browser/open") {
    const auto url = args->GetString("url").ToString();
    if (entry != session.tabs.end() || session.tabs.size() >= kMaxTabs) {
      return Error(409, "The tab exists or there are too many tabs");
    }
    if (!BrowserUrl(session, url)) {
      return Error(403, "The Browser may not open this address");
    }
    Tab tab;
    tab.incognito = args->GetBool("private");
    tab.url = url;
    session.tabs.emplace(id, std::move(tab));
    EnsureBrowserContext(session, args->GetBool("private"));
    return {};
  }
  if (entry == session.tabs.end() || entry->second.closing) {
    return Error(404, "No such tab");
  }
  Tab& tab = entry->second;
  if (command == "browser/bounds") {
    const int generation = args->GetType("generation") == VTYPE_INT ? args->GetInt("generation") : -1;
    if (generation < 0) {
      return Error(400, "The placement needs its generation");
    }
    if (generation <= tab.generation) {
      return {};  // A newer placement was applied already.
    }
    tab.generation = generation;
    tab.bounds = PlacementBounds(args);
    tab.visible = args->GetBool("visible");
    ApplyBounds(session, tab);
    return {};
  }
  if (command == "browser/navigate") {
    const auto url = args->GetString("url").ToString();
    if (!BrowserUrl(session, url)) {
      return Error(403, "The Browser may not open this address");
    }
    tab.url = url;
    if (tab.browser) {
      tab.browser->GetMainFrame()->LoadURL(url);
    }
    return {};
  }
  if (command == "browser/find") {
    const auto text = args->GetString("text").ToString();
    if (!tab.browser || text.empty() || text.size() > 1000) {
      return Error(400, "Nothing to find");
    }
    tab.browser->GetHost()->Find(text, !args->HasKey("forward") || args->GetBool("forward"),
                                 false, args->GetBool("next"));
    return {};
  }
  if (command == "browser/stop-find") {
    if (tab.browser) {
      tab.browser->GetHost()->StopFinding(args->GetBool("clear"));
    }
    return {};
  }
  if (command == "browser/zoom") {
    const double factor = args->GetType("factor") == VTYPE_INT ? args->GetInt("factor")
                                                                : args->GetDouble("factor");
    if (!tab.browser || !(factor >= 0.25 && factor <= 5)) {
      return Error(400, "Zoom is a factor from 0.25 to 5");
    }
    // Chromium's zoom levels are powers of 1.2.
    tab.browser->GetHost()->SetZoomLevel(std::log(factor) / std::log(1.2));
    return {};
  }
  if (command == "browser/action") {
    const auto action = args->GetString("action").ToString();
    if (action == "focus") {
      if (tab.overlay && tab.overlay->IsVisible() && session.window->IsActive()) {
        tab.view->RequestFocus();
      }
      return {};
    }
    if (!tab.browser) {
      return Error(409, "The tab is still opening");
    }
    if (action == "back") {
      tab.browser->GoBack();
    } else if (action == "forward") {
      tab.browser->GoForward();
    } else if (action == "reload") {
      tab.browser->Reload();
    } else if (action == "stop") {
      tab.browser->StopLoad();
    } else {
      return Error(400, "Unknown action");
    }
    return {};
  }
  if (command == "browser/close") {
    CloseTab(session, id);
    return {};
  }
  return Error(404, "Unknown command");
}

// Answers one request of the Desktop page to /_shell/ on the UI thread.
class ShellRequest final : public CefResourceHandler {
 public:
  ShellRequest(std::weak_ptr<Session> session, std::string command, bool allowed)
      : session_(std::move(session)), command_(std::move(command)), allowed_(allowed) {}

  bool Open(CefRefPtr<CefRequest> request, bool& handle_request,
            CefRefPtr<CefCallback> callback) override {
    handle_request = true;
    if (!allowed_) {
      reply_ = Error(403, "Only the Desktop page may use the shell");
      return true;
    }
    std::string body;
    if (auto post = request->GetPostData()) {
      CefPostData::ElementVector elements;
      post->GetElements(elements);
      for (const auto& element : elements) {
        const auto size = element->GetBytesCount();
        if (element->GetType() != PDE_TYPE_BYTES || body.size() + size > kMaxCommandBytes) {
          reply_ = Error(413, "The command is too large");
          return true;
        }
        std::string chunk(size, '\0');
        element->GetBytes(size, chunk.data());
        body += chunk;
      }
    }
    handle_request = false;
    CefPostTask(TID_UI, base::BindOnce(&ShellRequest::Run, CefRefPtr<ShellRequest>(this),
                                       std::move(body), callback));
    return true;
  }

  void GetResponseHeaders(CefRefPtr<CefResponse> response, int64_t& length,
                          CefString&) override {
    response->SetStatus(reply_.status);
    response->SetMimeType("application/json");
    response->SetHeaderByName("Cache-Control", "no-store", true);
    length = static_cast<int64_t>(reply_.body.size());
  }

  bool Read(void* data, int wanted, int& read,
            CefRefPtr<CefResourceReadCallback>) override {
    const auto left = reply_.body.size() - offset_;
    if (left == 0 || wanted <= 0) {
      read = 0;
      return false;
    }
    const auto count = std::min<std::size_t>(left, static_cast<std::size_t>(wanted));
    std::memcpy(data, reply_.body.data() + offset_, count);
    offset_ += count;
    read = static_cast<int>(count);
    return true;
  }

  void Cancel() override {}

 private:
  // The reply is written on the UI thread before Continue, which hands the
  // request back to the IO thread.
  static void Run(CefRefPtr<ShellRequest> self, std::string body,
                  CefRefPtr<CefCallback> callback) {
    Respond respond = [self, callback](Reply reply) {
      self->reply_ = std::move(reply);
      callback->Continue();
    };
    auto session = self->session_.lock();
    if (!session || session->closing) {
      respond(Error(503, "The session is ending"));
      return;
    }
    auto parsed = CefParseJSON(body.data(), body.size(), JSON_PARSER_RFC);
    auto args = parsed && parsed->GetType() == VTYPE_DICTIONARY ? parsed->GetDictionary()
                                                                 : CefDictionaryValue::Create();
    if (!features::Run(*session, self->command_, args, respond)) {
      respond(RunCommand(*session, self->command_, body));
    }
  }

  const std::weak_ptr<Session> session_;
  const std::string command_;
  const bool allowed_;
  Reply reply_;
  std::size_t offset_ = 0;
  IMPLEMENT_REFCOUNTING(ShellRequest);
};

class RequestContextHandler final : public CefRequestContextHandler,
                                    public CefResourceRequestHandler {
 public:
  RequestContextHandler(Startup startup, Kind kind, std::string shell_prefix,
                        std::weak_ptr<Session> session,
                        std::function<void()> initialized)
      : startup_(std::move(startup)), kind_(kind),
        shell_prefix_(std::move(shell_prefix)), session_(std::move(session)),
        initialized_(std::move(initialized)) {}

  // A context with an on-disk profile initializes asynchronously, and CEF
  // creates browser views only in initialized contexts.
  void OnRequestContextInitialized(CefRefPtr<CefRequestContext>) override {
    if (initialized_) {
      std::exchange(initialized_, nullptr)();
    }
  }

  CefRefPtr<CefResourceRequestHandler> GetResourceRequestHandler(
      CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, CefRefPtr<CefRequest>,
      bool, bool, const CefString&, bool&) override {
    return this;
  }

  ReturnValue OnBeforeResourceLoad(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>,
                                   CefRefPtr<CefRequest> request,
                                   CefRefPtr<CefCallback>) override {
    return Allows(startup_, kind_, OPEN_SHELL_RESOURCE, request->GetURL())
               ? RV_CONTINUE
               : RV_CANCEL;
  }

  // Only the Desktop page itself reaches the shell: not its frames, not the
  // Browser's tabs, which have their own contexts, and not a navigation.
  CefRefPtr<CefResourceHandler> GetResourceHandler(
      CefRefPtr<CefBrowser> browser, CefRefPtr<CefFrame> frame,
      CefRefPtr<CefRequest> request) override {
    if (shell_prefix_.empty()) {
      return nullptr;
    }
    const auto url = request->GetURL().ToString();
    if (url.compare(0, shell_prefix_.size(), shell_prefix_) != 0) {
      return nullptr;
    }
    auto command = url.substr(shell_prefix_.size());
    const auto session = session_.lock();
    const bool allowed =
        session && browser && frame && frame->IsMain() &&
        browser->GetIdentifier() == session->desktop_id.load() &&
        request->GetMethod() == "POST" && command.size() <= 32 &&
        std::all_of(command.begin(), command.end(), [](unsigned char c) {
          return (c >= 'a' && c <= 'z') || c == '/' || c == '-';
        });
    return new ShellRequest(session_, std::move(command), allowed);
  }

  void OnResourceRedirect(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>,
                          CefRefPtr<CefRequest>, CefRefPtr<CefResponse>,
                          CefString& target) override {
    if (!Allows(startup_, kind_, OPEN_SHELL_RESOURCE, target)) {
      target = "about:blank";
    }
  }

 private:
  const Startup startup_;
  const Kind kind_;
  const std::string shell_prefix_;
  const std::weak_ptr<Session> session_;
  std::function<void()> initialized_;
  IMPLEMENT_REFCOUNTING(RequestContextHandler);
};

void BrowserContextReady(std::weak_ptr<Session> weak, bool incognito,
                         CefRefPtr<CefRequestContext> context) {
  auto session = weak.lock();
  if (!session || session->closing) {
    return;
  }
  auto& current = incognito ? session->private_context : session->browser_context;
  if (!current || !current->IsSame(context)) {
    return;
  }
  (incognito ? session->private_ready : session->browser_ready) = true;
  features::BrowserContextReady(context);
  std::vector<std::string> waiting;
  for (const auto& [id, tab] : session->tabs) {
    if (tab.incognito == incognito && !tab.view) {
      waiting.push_back(id);
    }
  }
  for (const auto& id : waiting) {
    CreateTabView(*session, id);
  }
}

void EnsureBrowserContext(Session& session, bool incognito) {
  auto& context = incognito ? session.private_context : session.browser_context;
  if (context) {
    if (incognito ? session.private_ready : session.browser_ready) {
      std::vector<std::string> waiting;
      for (const auto& [id, tab] : session.tabs) {
        if (tab.incognito == incognito && !tab.view) {
          waiting.push_back(id);
        }
      }
      for (const auto& id : waiting) {
        CreateTabView(session, id);
      }
    }
    return;
  }
  CefRequestContextSettings settings;
  if (!incognito) {
    // The Browser keeps its logins in its own profile, apart from the Desktop.
    CefString(&settings.cache_path) = session.profile + "/browser";
  }
  std::weak_ptr<Session> weak = session.weak_from_this();
  // The handler learns its context once CreateContext returns.
  auto created = std::make_shared<CefRefPtr<CefRequestContext>>();
  auto ready = std::make_shared<bool>(false);
  context = CefRequestContext::CreateContext(
      settings, new RequestContextHandler(
                    session.startup, Kind::kBrowser, {}, weak,
                    [weak, incognito, created, ready] {
                      if (*created) {
                        CefPostTask(TID_UI, base::BindOnce(&BrowserContextReady, weak,
                                                           incognito,
                                                           std::exchange(*created, nullptr)));
                      } else {
                        *ready = true;
                      }
                    }));
  if (*ready) {
    CefPostTask(TID_UI, base::BindOnce(&BrowserContextReady, weak, incognito, context));
  } else {
    // Until it initializes. The handler then drops it.
    *created = context;
  }
}

class WindowDelegate final : public CefWindowDelegate {
 public:
  WindowDelegate(CefRefPtr<CefBrowserView> primary, CefRefPtr<CefBrowserView> guest,
                 bool fullscreen, std::shared_ptr<Session> session)
      : primary_(primary), guest_(guest), fullscreen_(fullscreen),
        session_(std::move(session)) {}

  void OnWindowCreated(CefRefPtr<CefWindow> window) override {
    session_->window = window;
    window->SetTitle(kTitle);
    if (guest_) {
      CefBoxLayoutSettings layout;
      layout.horizontal = true;
      layout.default_flex = 1;
      layout.cross_axis_alignment = CEF_AXIS_ALIGNMENT_STRETCH;
      window->SetToBoxLayout(layout);
    } else {
      window->SetToFillLayout();
    }
    window->AddChildView(primary_);
    if (guest_) {
      window->AddChildView(guest_);
    }
    window->Show();
    if (fullscreen_) {
      window->SetFullscreen(true);
    }
    primary_->RequestFocus();
  }

  // After a tab's overlay had the keyboard, Chromium can leave the keyboard
  // with no browser even though Views counts a browser as focused, and every
  // key then reaches the window unseen. Such keys go to that browser. A key
  // that a page left unhandled also reaches the window, and stays there.
  bool OnKeyEvent(CefRefPtr<CefWindow> window, const CefKeyEvent& event) override {
    auto& seen = session_->seen_keys;
    const auto now = std::chrono::steady_clock::now();
    while (!seen.empty() && now - seen.front().at > std::chrono::seconds(2)) {
      seen.pop_front();
    }
    const auto match = std::find_if(seen.begin(), seen.end(), [&](const SeenKey& key) {
      return key.type == event.type && key.code == event.windows_key_code;
    });
    if (match != seen.end()) {
      seen.erase(match);
      return false;
    }
    auto focused = window->GetFocusedView();
    auto view = focused ? focused->AsBrowserView() : nullptr;
    auto browser = view ? view->GetBrowser() : nullptr;
    if (!browser) {
      return false;
    }
    browser->GetHost()->SendKeyEvent(event);
    // The window gets no character events: a key that types text sends one.
    const bool command = (event.modifiers & (EVENTFLAG_CONTROL_DOWN | EVENTFLAG_ALT_DOWN)) &&
                         !(event.modifiers & EVENTFLAG_ALTGR_DOWN);
    const auto character = event.character;
    if (event.type == KEYEVENT_RAWKEYDOWN && !command &&
        ((character >= 0x20 && character != 0x7f) || character == 0x0d)) {
      CefKeyEvent typed = event;
      typed.type = KEYEVENT_CHAR;
      seen.push_back({typed.type, typed.windows_key_code, now});
      browser->GetHost()->SendKeyEvent(typed);
    }
    return true;
  }

  void OnWindowClosing(CefRefPtr<CefWindow>) override {
    if (guest_) {
      if (auto browser = guest_->GetBrowser()) {
        browser->GetHost()->CloseBrowser(true);
      }
    }
    CloseTabs(*session_);
  }

  void OnWindowDestroyed(CefRefPtr<CefWindow>) override {
    session_->window = nullptr;
    guest_ = nullptr;
    primary_ = nullptr;
  }

  bool CanClose(CefRefPtr<CefWindow>) override {
    if (!primary_) {
      return true;
    }
    auto browser = primary_->GetBrowser();
    return !browser || browser->GetHost()->TryCloseBrowser();
  }

  CefSize GetPreferredSize(CefRefPtr<CefView>) override { return {900, 640}; }

  cef_runtime_style_t GetWindowRuntimeStyle() override {
    return CEF_RUNTIME_STYLE_ALLOY;
  }

 private:
  CefRefPtr<CefBrowserView> primary_;
  CefRefPtr<CefBrowserView> guest_;
  const bool fullscreen_;
  const std::shared_ptr<Session> session_;
  IMPLEMENT_REFCOUNTING(WindowDelegate);
};

class DesktopApp final : public CefApp, public CefBrowserProcessHandler {
 public:
  DesktopApp(Startup startup, OpenshellCefStartupView config)
      : startup_(std::move(startup)), config_(config) {
    session_->startup = startup_;
    session_->profile = config_.profile;
    if (config_.desktop) {
      shell_prefix_ = "http://127.0.0.1:" +
                      std::to_string(openshell_cef_hostd_port(startup_.get())) +
                      kShellPath;
    }
  }

  CefRefPtr<CefBrowserProcessHandler> GetBrowserProcessHandler() override {
    return this;
  }

  void OnBeforeCommandLineProcessing(
      const CefString& type, CefRefPtr<CefCommandLine> command_line) override {
    if (type.empty()) {
      command_line->AppendSwitchWithValue("ozone-platform", "x11");
      // The image ships its own pinned Mesa. Chromium's blocklist, meant for
      // unknown drivers, otherwise refuses WebGL on its llvmpipe renderer.
      command_line->AppendSwitch("ignore-gpu-blocklist");
      // Verbose Chromium logs for the test fixtures. Child processes inherit
      // the switches. The Desktop never enables them.
      if (!config_.desktop && std::getenv("OPENSHELL_CEF_DIAGNOSTICS")) {
        command_line->AppendSwitchWithValue("enable-logging", "stderr");
        command_line->AppendSwitchWithValue("v", "1");
      }
    }
  }

  void OnContextInitialized() override {
    // One pending count per context, and one for this function, so that a
    // context that initializes synchronously cannot open the window early.
    pending_ = *config_.guest_url ? 3 : 2;
    context_ = CreateContext(Kind::kPrimary);
    if (!context_) {
      FailStartup("request context");
      return;
    }
    if (*config_.guest_url) {
      guest_context_ = CreateContext(Kind::kGuest);
      if (!guest_context_ || guest_context_->IsSharingWith(context_)) {
        FailStartup("guest request context");
        return;
      }
    }
    ContextReady();
  }

  bool failed() const { return session_->failed; }
  std::shared_ptr<Session> session() const { return session_; }

 private:
  CefRefPtr<CefRequestContext> CreateContext(Kind kind) {
    CefRequestContextSettings settings;
    if (kind == Kind::kPrimary && config_.desktop) {
      CefString(&settings.cache_path) = std::string(config_.profile) + "/desktop";
    }
    return CefRequestContext::CreateContext(
        settings, new RequestContextHandler(
                      startup_, kind, kind == Kind::kPrimary ? shell_prefix_ : "",
                      session_, [this] { ContextReady(); }));
  }

  void ContextReady() {
    if (session_->failed || --pending_ > 0) {
      return;
    }
    CefRefPtr<CefBrowserView> guest;
    if (guest_context_ && !(guest = CreateView(guest_context_, Kind::kGuest))) {
      FailStartup("guest view");
      return;
    }
    auto primary = CreateView(context_, Kind::kPrimary);
    if (!primary) {
      FailStartup("view");
      return;
    }
    if (!CefWindow::CreateTopLevelWindow(
            new WindowDelegate(primary, guest, config_.desktop, session_))) {
      FailStartup("window");
    }
  }

  CefRefPtr<CefBrowserView> CreateView(CefRefPtr<CefRequestContext> context,
                                       Kind kind) const {
    return CefBrowserView::CreateBrowserView(
        new BrowserClient(startup_, kind, config_.desktop, session_),
        kind == Kind::kGuest ? config_.guest_url : config_.primary_url,
        CefBrowserSettings(), nullptr, context, new ViewDelegate);
  }

  void FailStartup(const char* step) {
    session_->failed = true;
    std::cerr << "Unable to create the OpenShell CEF " << step << "\n";
    CefQuitMessageLoop();
  }

  const Startup startup_;
  const OpenshellCefStartupView config_;
  const std::shared_ptr<Session> session_ = [] {
    auto session = std::make_shared<Session>();
    session->features = features::CreateState();
    return session;
  }();
  std::string shell_prefix_;
  // Used on the UI thread only.
  CefRefPtr<CefRequestContext> context_;
  CefRefPtr<CefRequestContext> guest_context_;
  int pending_ = 0;
  IMPLEMENT_REFCOUNTING(DesktopApp);
};

}  // namespace

NO_STACK_PROTECTOR
int main(int argc, char* argv[]) {
  CefMainArgs args(argc, argv);
  const int child = CefExecuteProcess(args, nullptr, nullptr);
  if (child >= 0) {
    return child;
  }

  if (argc != 1 || geteuid() == 0) {
    std::cerr << "Run the OpenShell CEF host as an unprivileged user without arguments\n";
    return 1;
  }
  OpenshellCefStartupView config{};
  auto* startup = openshell_cef_startup(&config);
  if (!startup) {
    std::cerr << "Invalid OpenShell CEF startup configuration\n";
    return 1;
  }

  CefSettings settings;
  settings.no_sandbox = false;
  settings.command_line_args_disabled = true;
  settings.log_severity = config.desktop ? LOGSEVERITY_DISABLE : LOGSEVERITY_WARNING;
  if (!config.desktop) {
    CefString(&settings.log_file) = "/dev/stderr";
  }
  CefString(&settings.root_cache_path) = config.profile;

  CefRefPtr<DesktopApp> app =
      new DesktopApp(Startup(startup, openshell_cef_free), config);
  if (!CefInitialize(args, settings, app, nullptr)) {
    const int status = CefGetExitCode();
    std::cerr << "CEF initialization failed (" << status << ")\n";
    return status == 0 ? 1 : status;
  }

  if (config.desktop) {
    // The control thread owns its own copy of the Desktop origin.
    std::thread(ReadControl, openshell_cef_control(startup), app->session())
        .detach();
  }
  CefRunMessageLoop();
  const bool failed = app->failed();
  CefShutdown();
  return failed ? 1 : 0;
}
