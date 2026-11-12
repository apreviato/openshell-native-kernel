#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <functional>
#include <iostream>
#include <map>
#include <memory>
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
#include "include/cef_frame.h"
#include "include/cef_parser.h"
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
#include "startup.h"

#ifndef CEF_USE_SANDBOX
#error OpenShell requires a sandbox-enabled CEF build
#endif

namespace {

constexpr char kTitle[] = "OpenShell Native";
// Requests of the Desktop page that the host answers itself. They never reach hostd.
constexpr char kShellPath[] = "/_shell/";
constexpr char kBrowserEvent[] = "openshell-native-browser";
constexpr int kBridgeVersion = 1;
constexpr std::size_t kMaxCommandBytes = 64 * 1024;
constexpr std::size_t kMaxTabs = 64;
constexpr int kMaxViewSize = 16384;
constexpr std::size_t kMaxRendererRestarts = 3;
constexpr auto kRendererRestartWindow = std::chrono::minutes(1);

using Startup = std::shared_ptr<OpenshellCefStartup>;

// The browsing contexts of the host: the trusted page (the Desktop, or the
// fixture of a probe), the second fixture of the isolation test and the tabs
// of the Desktop's Browser.
enum class Kind { kPrimary, kGuest, kBrowser };

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

// A tab of the Desktop's Browser: a browser view over the Desktop page, at
// the place of its Browser window.
struct Tab {
  bool incognito = false;
  bool closing = false;
  bool visible = false;
  // The page numbers its placements; requests may arrive out of order.
  int generation = -1;
  CefRect bounds;
  std::string url;
  CefRefPtr<CefBrowserView> view;
  CefRefPtr<CefOverlayController> overlay;
  CefRefPtr<CefBrowser> browser;
};

// The state of the session, used on the browser process UI thread only,
// except desktop_id.
struct Session : std::enable_shared_from_this<Session> {
  Startup startup;
  std::string profile;
  int browsers = 0;
  bool failed = false;
  bool closing = false;
  CefRefPtr<CefBrowser> desktop;
  // Read on the IO thread to recognize requests of the Desktop page.
  std::atomic<int> desktop_id{0};
  CefRefPtr<CefWindow> window;
  std::string pending_bootstrap;
  std::deque<std::chrono::steady_clock::time_point> renderer_restarts;
  std::map<std::string, Tab> tabs;
  // The Browser's contexts are created with its first tab. The private one
  // lasts until the private session ends.
  CefRefPtr<CefRequestContext> browser_context;
  CefRefPtr<CefRequestContext> private_context;
  bool browser_ready = false;
  bool private_ready = false;
};

void CloseTab(Session& session, const std::string& id);
void CloseTabs(Session& session);

void CloseSession(std::shared_ptr<Session> session) {
  session->closing = true;
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

// Tells the Desktop page about one of its tabs.
void Emit(Session& session, CefRefPtr<CefDictionaryValue> event) {
  if (!session.desktop || session.closing) {
    return;
  }
  auto value = CefValue::Create();
  value->SetDictionary(event);
  const auto json = CefWriteJSON(value, JSON_WRITER_DEFAULT).ToString();
  session.desktop->GetMainFrame()->ExecuteJavaScript(
      std::string("window.dispatchEvent(new CustomEvent('") + kBrowserEvent +
          "',{detail:" + json + "}));",
      "", 0);
}

CefRefPtr<CefDictionaryValue> TabEvent(const std::string& id, const char* type) {
  auto event = CefDictionaryValue::Create();
  event->SetString("id", id);
  event->SetString("type", type);
  return event;
}

void FocusDesktop(Session& session) {
  if (session.desktop) {
    if (auto view = CefBrowserView::GetForBrowser(session.desktop)) {
      view->RequestFocus();
    }
  }
}

void ApplyBounds(Session& session, Tab& tab) {
  if (!tab.overlay) {
    return;
  }
  const bool shown = tab.visible && !tab.closing && tab.bounds.width > 0 &&
                     tab.bounds.height > 0;
  if (shown) {
    tab.overlay->SetBounds(tab.bounds);
  }
  if (tab.overlay->IsVisible() != shown) {
    tab.overlay->SetVisible(shown);
    if (!shown && tab.view && tab.view->HasFocus()) {
      FocusDesktop(session);
    }
  }
}

class ViewDelegate final : public CefBrowserViewDelegate {
 public:
  cef_runtime_style_t GetBrowserRuntimeStyle() override {
    return CEF_RUNTIME_STYLE_ALLOY;
  }

 private:
  IMPLEMENT_REFCOUNTING(ViewDelegate);
};

class BrowserClient final : public CefClient,
                            public CefDisplayHandler,
                            public CefLifeSpanHandler,
                            public CefLoadHandler,
                            public CefRequestHandler {
 public:
  BrowserClient(Startup startup, Kind kind, bool desktop,
                std::shared_ptr<Session> session, std::string tab = {})
      : startup_(std::move(startup)), kind_(kind), desktop_(desktop),
        session_(std::move(session)), tab_(std::move(tab)) {}

  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }

  bool OnBeforeBrowse(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                      CefRefPtr<CefRequest> request, bool, bool) override {
    const bool main = !frame || frame->IsMain();
    const auto url = request->GetURL();
    if (Allows(startup_, kind_, main ? OPEN_SHELL_MAIN_FRAME : OPEN_SHELL_SUBFRAME, url)) {
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
    if (IsDesktop() && frame && frame->IsMain()) {
      auto view = CefBrowserView::GetForBrowser(browser);
      auto window = view ? view->GetWindow() : nullptr;
      if (window && window->IsActive()) {
        // The view keeps its focus when only the renderer changed, and the
        // new renderer never learns it. Activating the window again restores
        // the focus to the page.
        window->Deactivate();
        window->Activate();
      }
      if (view) {
        view->RequestFocus();
      }
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
      }
      return;
    }
    if (!IsDesktop()) {
      return;
    }
    session_->desktop = browser;
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
      tab.url, CefBrowserSettings(), nullptr, context, new ViewDelegate);
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

// The commands of the Desktop page, from the requests to /_shell/.
struct Reply {
  int status = 200;
  std::string body = "{}";
};

Reply Error(int status, const char* message) {
  auto value = CefValue::Create();
  auto error = CefDictionaryValue::Create();
  error->SetString("error", message);
  value->SetDictionary(error);
  return {status, CefWriteJSON(value, JSON_WRITER_DEFAULT).ToString()};
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
    auto value = CefValue::Create();
    auto hello = CefDictionaryValue::Create();
    hello->SetInt("version", kBridgeVersion);
    hello->SetBool("browser", true);
    hello->SetString("event", kBrowserEvent);
    value->SetDictionary(hello);
    return {200, CefWriteJSON(value, JSON_WRITER_DEFAULT).ToString()};
  }
  auto parsed = CefParseJSON(body.data(), body.size(), JSON_PARSER_RFC);
  if (!parsed || parsed->GetType() != VTYPE_DICTIONARY) {
    return Error(400, "The command takes a JSON object");
  }
  auto args = parsed->GetDictionary();
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
    tab.bounds = CefRect(Coordinate(args, "x", -kMaxViewSize),
                         Coordinate(args, "y", -kMaxViewSize),
                         Coordinate(args, "width", 0), Coordinate(args, "height", 0));
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
  if (command == "browser/action") {
    const auto action = args->GetString("action").ToString();
    if (action == "focus") {
      if (tab.view && tab.visible) {
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
    auto session = self->session_.lock();
    self->reply_ = session && !session->closing
                       ? RunCommand(*session, self->command_, body)
                       : Error(503, "The session is ending");
    callback->Continue();
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
  const std::shared_ptr<Session> session_ = std::make_shared<Session>();
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
