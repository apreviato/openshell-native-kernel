// The state of a Desktop session in the CEF host, shared by main.cc and the
// Browser features (browser_features.cc). It belongs to the browser process UI
// thread, except Session::desktop_id.
#pragma once

#include <atomic>
#include <chrono>
#include <cstdint>
#include <deque>
#include <filesystem>
#include <functional>
#include <map>
#include <memory>
#include <string>
#include <vector>

#include "include/cef_browser.h"
#include "include/cef_request_context.h"
#include "include/cef_values.h"
#include "include/views/cef_browser_view.h"
#include "include/views/cef_overlay_controller.h"
#include "include/views/cef_window.h"
#include "clipboard.h"
#include "startup.h"

namespace openshell {

using Startup = std::shared_ptr<OpenshellCefStartup>;

// The browsing contexts of the host: the trusted page (the Desktop, or the
// fixture of a probe), the second fixture of the isolation test and the tabs
// of the Desktop's Browser.
enum class Kind { kPrimary, kGuest, kBrowser };

// A tab of the Desktop's Browser: a browser view over the Desktop page, at
// the place of its Browser window.
struct Tab {
  bool incognito = false;
  bool closing = false;
  bool visible = false;
  bool focus_on_show = false;
  // The page is in HTML fullscreen: the Desktop gives it the whole screen.
  bool fullscreen = false;
  // The page numbers its placements; requests may arrive out of order.
  int generation = -1;
  CefRect bounds;
  std::string url;
  CefRefPtr<CefBrowserView> view;
  CefRefPtr<CefOverlayController> overlay;
  CefRefPtr<CefBrowser> browser;
  // The tab's DevTools, docked over the Desktop where its page places them.
  CefRefPtr<CefBrowserView> devtools;
  CefRefPtr<CefWindow> devtools_window;
  bool devtools_focus = false;
};

// A key press that a focused tab hands to the Desktop page, which derives the
// list from its own shortcut definitions: the DOM key (lowercase) and the
// exact modifiers.
struct Shortcut {
  std::string key;
  int modifiers = 0;
  bool focus_desktop = false;
};

// A key event that a page left unhandled, to tell it from a lost key.
struct SeenKey {
  int type;
  int code;
  std::chrono::steady_clock::time_point at;
};

// The answer to a request of the Desktop page to /_shell/.
struct Reply {
  int status = 200;
  std::string body = "{}";
};

// Sends the reply of a request that completes later.
using Respond = std::function<void(Reply)>;

namespace features {
struct State;
}

struct Session : std::enable_shared_from_this<Session> {
  Startup startup;
  std::string profile;
  int browsers = 0;
  bool failed = false;
  bool closing = false;
  CefRefPtr<CefBrowser> desktop;
  // The latest key events that pages left unhandled.
  std::deque<SeenKey> seen_keys;
  // Read on the IO thread to recognize requests of the Desktop page.
  std::atomic<int> desktop_id{0};
  CefRefPtr<CefWindow> window;
  std::string pending_bootstrap;
  std::deque<std::chrono::steady_clock::time_point> renderer_restarts;
  std::map<std::string, Tab> tabs;
  int layout_generation = -1;
  // The key presses that go to the Desktop page even from a tab.
  std::vector<Shortcut> shortcuts;
  bool shortcut_phases = false;
  // The Browser's contexts are created with its first tab. The private one
  // lasts until the private session ends.
  CefRefPtr<CefRequestContext> browser_context;
  CefRefPtr<CefRequestContext> private_context;
  bool browser_ready = false;
  bool private_ready = false;
  // Downloads, questions, certificates and the other Browser features.
  std::shared_ptr<features::State> features;
  // The system clipboard, while the Desktop page exists.
  std::unique_ptr<ClipboardWatcher> clipboard;
};

// The DOM event that carries the host's events to the Desktop page.
inline constexpr char kBrowserEvent[] = "openshell-native-browser";

// Tells the Desktop page about the session or one of its tabs.
void Emit(Session& session, CefRefPtr<CefDictionaryValue> event);
CefRefPtr<CefDictionaryValue> TabEvent(const std::string& id, const char* type);

Reply Error(int status, const char* message);
// A JSON reply with the value.
Reply Result(CefRefPtr<CefValue> value);

// A file name for a download: the last path element, without leading dots,
// control characters or separators, and short enough for any filesystem.
std::string DownloadName(std::string name);
// The first free name in the folder: "name", "name (1)", ...
std::filesystem::path FreeDownloadPath(const std::filesystem::path& folder, const std::string& name);

}  // namespace openshell
