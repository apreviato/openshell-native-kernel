#include "browser_features.h"

#include <cctype>

#include <chrono>
#include <cstdlib>
#include <ctime>
#include <deque>
#include <filesystem>
#include <functional>
#include <map>
#include <set>
#include <system_error>
#include <utility>

#include "include/base/cef_callback.h"
#include "include/cef_cookie.h"
#include "include/cef_devtools_message_observer.h"
#include "include/cef_frame.h"
#include "include/cef_image.h"
#include "include/cef_keyboard_handler.h"
#include "include/cef_load_handler.h"
#include "include/cef_parser.h"
#include "include/cef_resource_request_handler.h"
#include "include/cef_ssl_info.h"
#include "include/cef_task.h"
#include "include/cef_x509_certificate.h"
#include "include/views/cef_button_delegate.h"
#include "include/views/cef_fill_layout.h"
#include "include/views/cef_label_button.h"
#include "include/views/cef_window_delegate.h"
#include "include/wrapper/cef_closure_task.h"
#include "input_state.h"

namespace openshell::features {
namespace {

using Clock = std::chrono::steady_clock;
using Answer = std::function<void(CefRefPtr<CefValue>)>;

// At most one progress report of a download in this time; its end always.
constexpr auto kProgressInterval = std::chrono::milliseconds(400);
// Unanswered questions are refused.
constexpr auto kMediaQuestionTimeout = std::chrono::minutes(2);
constexpr auto kFileQuestionTimeout = std::chrono::minutes(10);
constexpr std::size_t kMaxRefusedCertificates = 64;
constexpr std::size_t kMaxChosenFiles = 1000;
constexpr std::size_t kMaxFaviconUrls = 8;
constexpr int kFaviconSize = 64;
// The pictures the Desktop shows above the tabs: small parts of its page.
constexpr std::size_t kMaxOverlayPicture = 4 * 1024 * 1024;
constexpr int kMaxOverlaySize = 4096;
constexpr std::size_t kMaxOverlays = 8;
// The password manager's script (shared/passwordGuest.ts in the Desktop) and its messages.
constexpr std::size_t kMaxGuestScript = 512 * 1024;
constexpr std::size_t kMaxGuestMessage = 64 * 1024;
constexpr std::size_t kMaxGuestName = 64;

// The DevTools protocol methods that the Desktop's tools call on a tab: the
// agent reads, captures and operates pages, and Settings clears the cache.
const std::set<std::string> kProtocolMethods = {
    "Runtime.evaluate",        "Page.captureScreenshot",  "Page.printToPDF",
    "Input.dispatchMouseEvent", "Input.dispatchKeyEvent", "Input.insertText",
    "Network.clearBrowserCache",
};
// The methods the Desktop calls on its own page: a picture of it and input
// as the person's, for the agent's desktop tool.
const std::set<std::string> kShellProtocolMethods = {
    "Page.captureScreenshot", "Input.dispatchMouseEvent", "Input.dispatchKeyEvent", "Input.insertText",
};

struct Download {
  std::string tab;
  std::string id;
  std::string path;
  bool incognito = false;
  CefRefPtr<CefDownloadItemCallback> callback;
  Clock::time_point reported;
};

struct RefusedCertificate {
  std::string fingerprint;
  std::string issuer;
  std::string subject;
  double valid_start = 0;
  double valid_expiry = 0;
};

// A picture of a part of the Desktop's page, above the tabs' views: Views
// shows a picture in a button. A press on it goes back to the page, at the
// point where the pointer is, for the element drawn there (nativeShell.ts).
class OverlayPicture : public CefButtonDelegate {
 public:
  OverlayPicture(std::weak_ptr<Session> session, std::string name)
      : session_(std::move(session)), name_(std::move(name)) {}

  void OnButtonPressed(CefRefPtr<CefButton>) override {
    auto session = session_.lock();
    int x = 0;
    int y = 0;
    if (!session || !session->window || !PointerOnScreen(x, y)) {
      return;
    }
    const auto origin = session->window->GetClientAreaBoundsInScreen();
    auto event = TabEvent(std::string(), "overlay-press");
    event->SetString("name", name_);
    event->SetInt("x", x - origin.x);
    event->SetInt("y", y - origin.y);
    Emit(*session, event);
  }

 private:
  const std::weak_ptr<Session> session_;
  const std::string name_;
  IMPLEMENT_REFCOUNTING(OverlayPicture);
};

struct Overlay {
  CefRefPtr<CefOverlayController> controller;
  CefRefPtr<CefLabelButton> picture;
};

double Number(CefRefPtr<CefDictionaryValue> args, const char* key) {
  switch (args->GetType(key)) {
    case VTYPE_INT:
      return args->GetInt(key);
    case VTYPE_DOUBLE:
      return args->GetDouble(key);
    default:
      return 0;
  }
}

struct ProtocolCall {
  std::string tab;
  Respond respond;
};

}  // namespace

struct State {
  std::map<uint32_t, Download> downloads;
  int download_counter = 0;
  std::map<int, Answer> questions;
  int next_question = 1;
  // The person's answers about the camera and microphone, until the session ends.
  std::map<std::string, bool> media_answers;
  std::map<std::string, RefusedCertificate> refused;
  std::deque<std::string> refused_order;
  // "host fingerprint" of each certificate the person allowed.
  std::set<std::string> exceptions;
  std::map<std::string, CefRefPtr<CefRegistration>> observers;
  std::map<int, ProtocolCall> protocol_calls;
  int protocol_counter = 0;
  int pdf_counter = 0;
  // Parts of the Desktop's page shown above the tabs, by the page's name for them.
  std::map<std::string, Overlay> overlays;
  // The password manager's script, which the Desktop gives, runs in this
  // isolated world of each tab's top frame: it calls the binding, which only
  // that world has, and the host calls the receiver there.
  std::string guest_script;
  std::string guest_world;
  std::string guest_binding;
  std::string guest_receiver;
  std::set<std::string> guest_tabs;
};

std::shared_ptr<State> CreateState() { return std::make_shared<State>(); }

namespace {

State& StateOf(Session& session) { return *session.features; }

Tab* FindTab(Session& session, const std::string& id) {
  auto entry = session.tabs.find(id);
  return entry == session.tabs.end() || entry->second.closing ? nullptr : &entry->second;
}

// "host" or "host:port", as the Desktop names sites (new URL(...).host).
std::string SiteOf(const CefString& url) {
  CefURLParts parts;
  if (!CefParseURL(url, parts)) {
    return {};
  }
  const auto host = CefString(&parts.host).ToString();
  const auto port = CefString(&parts.port).ToString();
  return port.empty() ? host : host + ":" + port;
}

CefRefPtr<CefValue> StringValue(const std::string& value) {
  auto result = CefValue::Create();
  result->SetString(value);
  return result;
}

CefRefPtr<CefValue> Bool(bool value) {
  auto result = CefValue::Create();
  result->SetBool(value);
  return result;
}

CefRefPtr<CefValue> Null() {
  auto result = CefValue::Create();
  result->SetNull();
  return result;
}

CefRefPtr<CefValue> Dictionary(CefRefPtr<CefDictionaryValue> dictionary) {
  auto result = CefValue::Create();
  result->SetDictionary(dictionary);
  return result;
}

double Seconds(const CefBaseTime& time) {
  cef_time_t exploded;
  time_t seconds = 0;
  if (cef_time_from_basetime(time, &exploded) && cef_time_to_timet(&exploded, &seconds)) {
    return static_cast<double>(seconds);
  }
  return 0;
}

// ── questions ──────────────────────────────────────────────────────────────

void AnswerQuestion(Session& session, int id, CefRefPtr<CefValue> value) {
  auto& questions = StateOf(session).questions;
  auto entry = questions.find(id);
  if (entry == questions.end()) {
    return;
  }
  auto answer = std::move(entry->second);
  questions.erase(entry);
  answer(value ? value : Null());
}

// Asks the Desktop page, which shows its own dialog. No answer in time is no.
void Ask(Session& session, const char* kind, CefRefPtr<CefDictionaryValue> question,
         std::chrono::milliseconds timeout, Answer answer) {
  auto& state = StateOf(session);
  const int id = state.next_question++;
  state.questions[id] = std::move(answer);
  question->SetString("id", "");
  question->SetString("type", "ask");
  question->SetInt("question", id);
  question->SetString("kind", kind);
  Emit(session, question);
  CefPostDelayedTask(TID_UI,
                     base::BindOnce(
                         [](std::weak_ptr<Session> weak, int id) {
                           if (auto session = weak.lock()) {
                             AnswerQuestion(*session, id, nullptr);
                           }
                         },
                         session.weak_from_this(), id),
                     timeout.count());
}

// ── downloads ──────────────────────────────────────────────────────────────

void ReportDownload(Session& session, const Download& download, CefRefPtr<CefDownloadItem> item,
                    const char* state) {
  auto info = CefDictionaryValue::Create();
  info->SetString("id", download.id);
  info->SetString("state", state);
  info->SetString("filename", std::filesystem::path(download.path).filename().string());
  info->SetString("path", download.path);
  info->SetString("url", item->GetURL());
  info->SetDouble("receivedBytes", static_cast<double>(item->GetReceivedBytes()));
  info->SetDouble("totalBytes", static_cast<double>(item->GetTotalBytes()));
  info->SetBool("paused", item->IsPaused());
  if (download.incognito) {
    info->SetBool("private", true);
  }
  auto event = TabEvent(download.tab, "download");
  event->SetDictionary("download", info);
  Emit(session, event);
}

// ── DevTools ───────────────────────────────────────────────────────────────

void ReportDevTools(Session& session, const std::string& tab, bool open) {
  auto event = TabEvent(tab, "devtools");
  event->SetBool("open", open);
  Emit(session, event);
}

void CloseDevTools(Session& session, const std::string& id, Tab& tab) {
  if (!tab.devtools_window) {
    return;
  }
  auto window = std::exchange(tab.devtools_window, nullptr);
  tab.devtools = nullptr;
  tab.devtools_focus = false;
  window->Hide();
  window->Close();
  ReportDevTools(session, id, false);
  if (!session.closing && !tab.closing && tab.overlay && tab.overlay->IsVisible()) {
    tab.view->RequestFocus();
    tab.browser->GetHost()->SetFocus(true);
  }
}

// DevTools uses a Chrome-style BrowserView, which needs its own Chrome-style
// CefWindow. Keep that window attached to the Desktop and move it with the dock.
class DevToolsWindow final : public CefWindowDelegate {
 public:
  DevToolsWindow(std::weak_ptr<Session> session, std::string id,
                 CefRefPtr<CefBrowserView> view)
      : session_(std::move(session)), id_(std::move(id)), view_(view) {}

  cef_runtime_style_t GetWindowRuntimeStyle() override { return CEF_RUNTIME_STYLE_CHROME; }
  bool IsFrameless(CefRefPtr<CefWindow>) override { return true; }

  CefRefPtr<CefWindow> GetParentWindow(CefRefPtr<CefWindow>, bool*, bool*) override {
    const auto session = session_.lock();
    return session ? session->window : nullptr;
  }

  void OnWindowCreated(CefRefPtr<CefWindow> window) override {
    window->SetToFillLayout();
    window->AddChildView(view_);
  }

  void OnWindowActivationChanged(CefRefPtr<CefWindow>, bool active) override {
    if (auto session = session_.lock(); session && active) {
      Emit(*session, TabEvent(id_, "devtools-pointer"));
    }
  }

  void OnWindowDestroyed(CefRefPtr<CefWindow> window) override {
    if (auto session = session_.lock()) {
      auto entry = session->tabs.find(id_);
      if (entry != session->tabs.end() && entry->second.devtools_window &&
          entry->second.devtools_window->IsSame(window)) {
        entry->second.devtools_window = nullptr;
        entry->second.devtools = nullptr;
        entry->second.devtools_focus = false;
        ReportDevTools(*session, id_, false);
      }
    }
    view_ = nullptr;
  }

 private:
  const std::weak_ptr<Session> session_;
  const std::string id_;
  CefRefPtr<CefBrowserView> view_;
  IMPLEMENT_REFCOUNTING(DevToolsWindow);
};

bool DevToolsUrl(const CefString& url) {
  return url.ToString().rfind("devtools://devtools/", 0) == 0;
}

class DevToolsClient final : public CefClient,
                             public CefKeyboardHandler,
                             public CefLoadHandler,
                             public CefRequestHandler,
                             public CefResourceRequestHandler {
 public:
  DevToolsClient(std::weak_ptr<Session> session, std::string id, std::string action)
      : session_(std::move(session)), id_(std::move(id)), action_(std::move(action)) {}

  CefRefPtr<CefKeyboardHandler> GetKeyboardHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }

  bool OnBeforeBrowse(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                      CefRefPtr<CefRequest> request, bool, bool) override {
    return frame->IsMain() && !DevToolsUrl(request->GetURL());
  }

  // The frontend shares the inspected tab's request context. Override its
  // scheme filter only for Chromium's bundled DevTools resources.
  CefRefPtr<CefResourceRequestHandler> GetResourceRequestHandler(
      CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, CefRefPtr<CefRequest> request,
      bool, bool, const CefString&, bool&) override {
    return DevToolsUrl(request->GetURL()) ? this : nullptr;
  }

  bool OnPreKeyEvent(CefRefPtr<CefBrowser> browser, const CefKeyEvent& event, CefEventHandle, bool*) override {
    const auto modifiers = event.modifiers & (EVENTFLAG_SHIFT_DOWN | EVENTFLAG_CONTROL_DOWN |
        EVENTFLAG_ALT_DOWN | EVENTFLAG_COMMAND_DOWN | EVENTFLAG_ALTGR_DOWN);
    if (event.type != KEYEVENT_RAWKEYDOWN) {
      return false;
    }
    const bool ctrl_shift = modifiers == (EVENTFLAG_CONTROL_DOWN | EVENTFLAG_SHIFT_DOWN);
    if (ctrl_shift && (event.windows_key_code == 'J' || event.windows_key_code == 'C')) {
      RunAction(browser->GetMainFrame(), event.windows_key_code == 'J' ? "console" : "inspect");
      return true;
    }
    if (!((event.windows_key_code == 0x7b && modifiers == 0) || (event.windows_key_code == 'I' && ctrl_shift))) {
      return false;
    }
    CefPostTask(TID_UI, base::BindOnce([](std::weak_ptr<Session> weak, std::string id) {
      if (auto session = weak.lock()) {
        auto tab = session->tabs.find(id);
        if (tab != session->tabs.end()) {
          CloseDevTools(*session, id, tab->second);
        }
      }
    }, session_, id_));
    return true;
  }

  void OnLoadEnd(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame, int) override {
    if (frame->IsMain() && DevToolsUrl(frame->GetURL())) {
      RunAction(frame, std::exchange(action_, {}), true);
    }
  }

  static void RunAction(CefRefPtr<CefFrame> frame, const std::string& action, bool starting = false) {
    if (!frame || !DevToolsUrl(frame->GetURL())) {
      return;
    }
    const char* command = action == "console" ? "DevToolsAPI.showPanel('console')" :
        action == "inspect" ? "DevToolsAPI.enterInspectElementMode()" : nullptr;
    if (!command) {
      return;
    }
    if (starting) {
      // The frontend finishes wiring its controls after the document loads.
      frame->ExecuteJavaScript(std::string("setTimeout(() => { ") + command + "; }, 750)", "", 0);
    } else {
      frame->ExecuteJavaScript(command, "", 0);
    }
  }

 private:
  const std::weak_ptr<Session> session_;
  const std::string id_;
  std::string action_;
  IMPLEMENT_REFCOUNTING(DevToolsClient);
};

// ── the DevTools protocol ──────────────────────────────────────────────────

void GuestCalled(Session& session, const std::string& id, CefRefPtr<CefBrowser> browser,
                 const void* params, size_t size);
void GuestNavigated(Session& session, const std::string& id, const void* params, size_t size);

class ProtocolObserver final : public CefDevToolsMessageObserver {
 public:
  ProtocolObserver(std::weak_ptr<Session> session, std::string id)
      : session_(std::move(session)), id_(std::move(id)) {}

  void OnDevToolsEvent(CefRefPtr<CefBrowser> browser, const CefString& method, const void* params,
                       size_t size) override {
    auto session = session_.lock();
    if (!session || id_.empty()) {
      return;
    }
    if (method == "Runtime.bindingCalled") {
      GuestCalled(*session, id_, browser, params, size);
    } else if (method == "Page.frameNavigated") {
      GuestNavigated(*session, id_, params, size);
    }
  }

  void OnDevToolsMethodResult(CefRefPtr<CefBrowser>, int message_id, bool success,
                              const void* result, size_t size) override {
    auto session = session_.lock();
    if (!session) {
      return;
    }
    auto& calls = StateOf(*session).protocol_calls;
    auto entry = calls.find(message_id);
    if (entry == calls.end()) {
      return;
    }
    auto respond = std::move(entry->second.respond);
    calls.erase(entry);
    auto value = CefParseJSON(result, size, JSON_PARSER_RFC);
    if (!value) {
      respond(Error(502, "The page answered with no result"));
    } else if (!success) {
      const auto message = value->GetType() == VTYPE_DICTIONARY
                               ? value->GetDictionary()->GetString("message").ToString()
                               : std::string();
      respond(Error(502, message.empty() ? "The page refused the call" : message.c_str()));
    } else {
      respond(Result(value));
    }
  }

 private:
  const std::weak_ptr<Session> session_;
  const std::string id_;
  IMPLEMENT_REFCOUNTING(ProtocolObserver);
};

// Calls a method on a page: a tab's (`id`) or the Desktop's (`id` empty).
// False when the page cannot take it.
bool CallProtocol(Session& session, const std::string& id, CefRefPtr<CefBrowser> browser,
                  const std::string& method, CefRefPtr<CefDictionaryValue> params, Respond respond) {
  if (!browser) {
    return false;
  }
  auto& state = StateOf(session);
  if (!state.observers.count(id)) {
    state.observers[id] =
        browser->GetHost()->AddDevToolsMessageObserver(new ProtocolObserver(session.weak_from_this(), id));
  }
  const int message = ++state.protocol_counter;
  state.protocol_calls[message] = {id, std::move(respond)};
  if (browser->GetHost()->ExecuteDevToolsMethod(message, method, params) == 0) {
    state.protocol_calls.erase(message);
    return false;
  }
  return true;
}

using ProtocolNext = std::function<void(CefRefPtr<CefDictionaryValue>)>;

// Calls a method and goes on with its result; `fail` answers otherwise.
void ProtocolThen(Session& session, const std::string& id, CefRefPtr<CefBrowser> browser,
                  const char* method, CefRefPtr<CefDictionaryValue> params, Respond fail,
                  ProtocolNext next) {
  auto failed = fail;
  const bool called = CallProtocol(
      session, id, browser, method, params,
      [fail = std::move(fail), next = std::move(next)](Reply reply) {
        if (reply.status != 200) {
          fail(reply);
          return;
        }
        auto value = CefParseJSON(reply.body, JSON_PARSER_RFC);
        if (!value || value->GetType() != VTYPE_DICTIONARY) {
          fail(Error(502, "The page answered with no result"));
          return;
        }
        next(value->GetDictionary());
      });
  if (!called) {
    failed(Error(409, "The tab is still opening"));
  }
}

// ── the password manager's script ──────────────────────────────────────────

// "scheme://host[:port]", as the page's location.origin says it.
std::string OriginOf(const CefString& url) {
  CefURLParts parts;
  if (!CefParseURL(url, parts)) {
    return {};
  }
  const auto scheme = CefString(&parts.scheme).ToString();
  const auto host = CefString(&parts.host).ToString();
  const auto port = CefString(&parts.port).ToString();
  if (scheme.empty() || host.empty()) {
    return {};
  }
  return scheme + "://" + host + (port.empty() ? "" : ":" + port);
}

// A name the script and the host share: a JavaScript identifier.
bool GuestName(const std::string& name) {
  if (name.empty() || name.size() > kMaxGuestName || std::isdigit(static_cast<unsigned char>(name[0]))) {
    return false;
  }
  for (const char character : name) {
    if (!std::isalnum(static_cast<unsigned char>(character)) && character != '_' && character != '$' &&
        character != '-') {
      return false;
    }
  }
  return true;
}

// Runs the script in the tab's page: in its world of the top frame, with
// the binding added to that world's context only. Without the Runtime
// domain enabled (pages can notice it), each document gets it again when it
// comes (GuestNavigated); the script runs once per document.
void RunGuest(Session& session, const std::string& id, const std::string& frame) {
  auto& state = StateOf(session);
  Tab* tab = FindTab(session, id);
  if (state.guest_script.empty() || !tab || !tab->browser) {
    return;
  }
  auto browser = tab->browser;
  auto ignore = [](Reply) {};
  std::weak_ptr<Session> weak = session.weak_from_this();
  auto world = [weak, id, browser, ignore](const std::string& frame_id) {
    auto session = weak.lock();
    if (!session || frame_id.empty()) {
      return;
    }
    auto& state = StateOf(*session);
    auto params = CefDictionaryValue::Create();
    params->SetString("frameId", frame_id);
    params->SetString("worldName", state.guest_world);
    ProtocolThen(*session, id, browser, "Page.createIsolatedWorld", params, ignore,
                 [weak, id, browser, ignore](CefRefPtr<CefDictionaryValue> created) {
                   auto session = weak.lock();
                   if (!session) {
                     return;
                   }
                   auto& state = StateOf(*session);
                   const int context = created->GetInt("executionContextId");
                   auto binding = CefDictionaryValue::Create();
                   binding->SetString("name", state.guest_binding);
                   binding->SetInt("executionContextId", context);
                   CallProtocol(*session, id, browser, "Runtime.addBinding", binding, ignore);
                   auto script = CefDictionaryValue::Create();
                   script->SetString("expression", state.guest_script);
                   script->SetInt("contextId", context);
                   CallProtocol(*session, id, browser, "Runtime.evaluate", script, ignore);
                 });
  };
  if (!frame.empty()) {
    world(frame);
    return;
  }
  ProtocolThen(session, id, browser, "Page.getFrameTree", nullptr, ignore,
               [world](CefRefPtr<CefDictionaryValue> tree) {
                 auto root = tree->GetDictionary("frameTree");
                 auto top = root ? root->GetDictionary("frame") : nullptr;
                 if (top) {
                   world(top->GetString("id").ToString());
                 }
               });
}

// Puts the script in the tab: in its page now, and in each page it opens.
void InstallGuest(Session& session, const std::string& id) {
  auto& state = StateOf(session);
  Tab* tab = FindTab(session, id);
  if (state.guest_script.empty() || !tab || !tab->browser || state.guest_tabs.count(id)) {
    return;
  }
  state.guest_tabs.insert(id);
  // The Page domain tells of each new document (GuestNavigated).
  CallProtocol(session, id, tab->browser, "Page.enable", nullptr, [](Reply) {});
  RunGuest(session, id, std::string());
}

// A new document in the tab's top frame: the script goes into it.
void GuestNavigated(Session& session, const std::string& id, const void* params, size_t size) {
  if (!StateOf(session).guest_tabs.count(id)) {
    return;
  }
  auto event = CefParseJSON(params, size, JSON_PARSER_RFC);
  auto frame = event && event->GetType() == VTYPE_DICTIONARY ? event->GetDictionary()->GetDictionary("frame") : nullptr;
  if (!frame || frame->HasKey("parentId")) {
    return;
  }
  RunGuest(session, id, frame->GetString("id").ToString());
}

// The script in a tab said something: the Desktop hears it with the origin of
// the tab's top frame, as the browser knows it. A message that names another
// origin (the page navigated meanwhile) is dropped.
void GuestCalled(Session& session, const std::string& id, CefRefPtr<CefBrowser> browser,
                 const void* params, size_t size) {
  auto& state = StateOf(session);
  Tab* tab = FindTab(session, id);
  if (!tab || !tab->browser || !tab->browser->IsSame(browser) || state.guest_binding.empty() ||
      size > kMaxGuestMessage * 2) {
    return;
  }
  auto call = CefParseJSON(params, size, JSON_PARSER_RFC);
  if (!call || call->GetType() != VTYPE_DICTIONARY ||
      call->GetDictionary()->GetString("name").ToString() != state.guest_binding) {
    return;
  }
  const auto payload = call->GetDictionary()->GetString("payload").ToString();
  auto said = payload.size() <= kMaxGuestMessage ? CefParseJSON(payload, JSON_PARSER_RFC) : nullptr;
  if (!said || said->GetType() != VTYPE_DICTIONARY) {
    return;
  }
  const auto origin = OriginOf(browser->GetMainFrame()->GetURL());
  if (origin.empty() || said->GetDictionary()->GetString("origin").ToString() != origin) {
    return;
  }
  auto message = said->GetDictionary()->GetValue("message");
  if (!message || message->GetType() != VTYPE_DICTIONARY) {
    return;
  }
  auto event = TabEvent(id, "guest");
  event->SetString("origin", origin);
  event->SetBool("private", tab->incognito);
  event->SetValue("message", message->Copy());
  Emit(session, event);
}

// ── favicons ───────────────────────────────────────────────────────────────

// Tries each address in turn and answers with the first picture.
class FaviconFetch final : public CefDownloadImageCallback {
 public:
  FaviconFetch(CefRefPtr<CefBrowser> browser, std::vector<std::string> urls, Respond respond)
      : browser_(std::move(browser)), urls_(std::move(urls)), respond_(std::move(respond)) {}

  void Next() {
    if (next_ >= urls_.size() || !browser_->IsValid()) {
      Finish(Null());
      return;
    }
    browser_->GetHost()->DownloadImage(urls_[next_++], true, kFaviconSize, false, this);
  }

  void OnDownloadImageFinished(const CefString&, int, CefRefPtr<CefImage> image) override {
    int width = 0;
    int height = 0;
    auto png = image && !image->IsEmpty() ? image->GetAsPNG(1.0f, true, width, height) : nullptr;
    if (!png || png->GetSize() == 0) {
      Next();
      return;
    }
    std::string bytes(png->GetSize(), '\0');
    png->GetData(bytes.data(), bytes.size(), 0);
    auto url = CefValue::Create();
    url->SetString("data:image/png;base64," + CefBase64Encode(bytes.data(), bytes.size()).ToString());
    Finish(url);
  }

 private:
  void Finish(CefRefPtr<CefValue> url) {
    auto result = CefDictionaryValue::Create();
    result->SetValue("url", url);
    respond_(Result(Dictionary(result)));
  }

  const CefRefPtr<CefBrowser> browser_;
  const std::vector<std::string> urls_;
  const Respond respond_;
  std::size_t next_ = 0;
  IMPLEMENT_REFCOUNTING(FaviconFetch);
};

// ── printing ───────────────────────────────────────────────────────────────

class PdfDone final : public CefPdfPrintCallback {
 public:
  explicit PdfDone(Respond respond) : respond_(std::move(respond)) {}

  void OnPdfPrintFinished(const CefString& path, bool ok) override {
    if (!ok) {
      respond_(Error(500, "The page could not be printed"));
      return;
    }
    auto result = CefValue::Create();
    result->SetString(path);
    respond_(Result(result));
  }

 private:
  const Respond respond_;
  IMPLEMENT_REFCOUNTING(PdfDone);
};

// A tab that belongs to the private or the normal context, with its page.
Tab* AnyTab(Session& session, bool incognito) {
  for (auto& [id, tab] : session.tabs) {
    if (!tab.closing && tab.browser && tab.incognito == incognito) {
      return &tab;
    }
  }
  return nullptr;
}

}  // namespace

bool BeforeDownload(Session& session, const std::string& tab, CefRefPtr<CefDownloadItem> item,
                    const CefString& suggested_name, CefRefPtr<CefBeforeDownloadCallback> callback) {
  const char* home = std::getenv("HOME");
  if (!home || *home != '/') {
    return false;
  }
  std::error_code error;
  const auto folder = std::filesystem::path(home) / "Downloads";
  std::filesystem::create_directories(folder, error);
  if (error) {
    return false;
  }
  const auto path = FreeDownloadPath(folder, DownloadName(suggested_name.ToString()));
  callback->Continue(path.string(), false);
  auto& state = StateOf(session);
  auto& download = state.downloads[item->GetId()];
  download.tab = tab;
  download.id = "download-" + std::to_string(++state.download_counter);
  download.path = path.string();
  const Tab* owner = FindTab(session, tab);
  download.incognito = owner && owner->incognito;
  download.reported = Clock::now();
  ReportDownload(session, download, item, "started");
  return true;
}

void DownloadUpdated(Session& session, CefRefPtr<CefDownloadItem> item,
                     CefRefPtr<CefDownloadItemCallback> callback) {
  auto& downloads = StateOf(session).downloads;
  auto entry = downloads.find(item->GetId());
  if (entry == downloads.end()) {
    return;
  }
  Download& download = entry->second;
  download.callback = callback;
  if (item->IsComplete() || item->IsCanceled()) {
    ReportDownload(session, download, item, item->IsComplete() ? "completed" : "cancelled");
    downloads.erase(entry);
    return;
  }
  if (item->IsInterrupted()) {
    ReportDownload(session, download, item, "interrupted");
    return;
  }
  const auto now = Clock::now();
  if (now - download.reported >= kProgressInterval) {
    download.reported = now;
    ReportDownload(session, download, item, "progressing");
  }
}

bool MediaAccess(Session& session, const std::string& tab, const CefString& origin,
                 uint32_t permissions, CefRefPtr<CefMediaAccessCallback> callback) {
  constexpr uint32_t kAudio = CEF_MEDIA_PERMISSION_DEVICE_AUDIO_CAPTURE;
  constexpr uint32_t kVideo = CEF_MEDIA_PERMISSION_DEVICE_VIDEO_CAPTURE;
  // Capturing the screen is not offered to pages.
  if ((permissions & ~(kAudio | kVideo)) != 0 || (permissions & (kAudio | kVideo)) == 0) {
    callback->Cancel();
    return true;
  }
  const bool audio = permissions & kAudio;
  const bool video = permissions & kVideo;
  const std::string wants = audio && video ? "camera and microphone" : video ? "camera" : "microphone";
  const Tab* owner = FindTab(session, tab);
  const std::string site = SiteOf(origin);
  // A private window's answers are its own.
  const std::string key = (owner && owner->incognito ? "private|" : "") + site + "|" + wants;
  auto finish = [callback, permissions](bool allowed) {
    if (allowed) {
      callback->Continue(permissions);
    } else {
      callback->Cancel();
    }
  };
  auto& answers = StateOf(session).media_answers;
  if (auto known = answers.find(key); known != answers.end()) {
    finish(known->second);
    return true;
  }
  auto question = CefDictionaryValue::Create();
  question->SetString("site", site);
  question->SetString("wants", wants);
  Ask(session, "media", question, kMediaQuestionTimeout,
      [weak = session.weak_from_this(), key, finish](CefRefPtr<CefValue> value) {
        const bool answered = value->GetType() == VTYPE_BOOL;
        if (answered) {
          if (auto session = weak.lock()) {
            StateOf(*session).media_answers[key] = value->GetBool();
          }
        }
        finish(answered && value->GetBool());
      });
  return true;
}

bool PermissionPrompt(Session&, const std::string&, uint32_t,
                      CefRefPtr<CefPermissionPromptCallback> callback) {
  callback->Continue(CEF_PERMISSION_RESULT_DENY);
  return true;
}

bool FileDialog(Session& session, const CefString& page, CefDialogHandler::FileDialogMode mode,
                const std::vector<CefString>& accept, CefRefPtr<CefFileDialogCallback> callback) {
  if (mode != FILE_DIALOG_OPEN && mode != FILE_DIALOG_OPEN_MULTIPLE) {
    // Pages save through downloads; folders are not offered.
    callback->Cancel();
    return true;
  }
  std::string filters;
  for (const auto& filter : accept) {
    filters += (filters.empty() ? "" : ",") + filter.ToString();
  }
  auto question = CefDictionaryValue::Create();
  question->SetBool("multiple", mode == FILE_DIALOG_OPEN_MULTIPLE);
  question->SetString("site", page.empty() ? std::string() : SiteOf(page));
  question->SetString("accept", filters.substr(0, 500));
  Ask(session, "choose-files", question, kFileQuestionTimeout, [callback](CefRefPtr<CefValue> value) {
    std::vector<CefString> paths;
    auto list = value->GetType() == VTYPE_LIST ? value->GetList() : nullptr;
    for (std::size_t index = 0; list && index < list->GetSize() && index < kMaxChosenFiles; ++index) {
      const auto path = list->GetString(index).ToString();
      if (path.empty() || path.front() != '/') {
        paths.clear();
        break;
      }
      paths.emplace_back(path);
    }
    if (paths.empty()) {
      callback->Cancel();
    } else {
      callback->Continue(paths);
    }
  });
  return true;
}

bool CertificateError(Session& session, const CefString& url, CefRefPtr<CefSSLInfo> ssl,
                      CefRefPtr<CefCallback> callback) {
  CefURLParts parts;
  if (!CefParseURL(url, parts) || CefString(&parts.scheme).ToString() != "https") {
    return false;
  }
  auto certificate = ssl ? ssl->GetX509Certificate() : nullptr;
  auto der = certificate ? certificate->GetDEREncoded() : nullptr;
  if (!der || der->GetSize() == 0) {
    return false;
  }
  std::string fingerprint(der->GetSize(), '\0');
  der->GetData(fingerprint.data(), fingerprint.size(), 0);
  const auto host = SiteOf(url);
  auto& state = StateOf(session);
  if (state.exceptions.count(host + " " + fingerprint)) {
    callback->Continue();
    return true;
  }
  // The Desktop shows what was refused and can allow it.
  auto& refused = state.refused[host];
  refused.fingerprint = fingerprint;
  refused.issuer = certificate->GetIssuer()->GetDisplayName().ToString();
  refused.subject = certificate->GetSubject()->GetDisplayName().ToString();
  refused.valid_start = Seconds(certificate->GetValidStart());
  refused.valid_expiry = Seconds(certificate->GetValidExpiry());
  state.refused_order.push_back(host);
  while (state.refused.size() > kMaxRefusedCertificates && !state.refused_order.empty()) {
    state.refused.erase(state.refused_order.front());
    state.refused_order.pop_front();
  }
  return false;
}

bool DevToolsCreated(Session& session, const std::string& id, CefRefPtr<CefBrowserView> devtools) {
  Tab* tab = FindTab(session, id);
  if (!tab || !session.window || tab->devtools_window) {
    return false;
  }
  auto window = CefWindow::CreateTopLevelWindow(new DevToolsWindow(session.weak_from_this(), id, devtools));
  if (!window) {
    return false;
  }
  tab->devtools_window = window;
  tab->devtools = devtools;
  ReportDevTools(session, id, true);
  return true;
}

void ClipboardCopied(Session& session, const ClipboardCopy& copy) {
  auto event = TabEvent(std::string(), "clipboard");
  event->SetString("kind", copy.kind);
  if (copy.kind == "image") {
    event->SetString("png", CefBase64Encode(copy.data.data(), copy.data.size()));
  } else {
    event->SetString("text", copy.data);
  }
  auto formats = CefListValue::Create();
  for (const auto& format : copy.formats) {
    formats->SetString(formats->GetSize(), format);
  }
  event->SetList("formats", formats);
  Emit(session, event);
}

void BrowserContextReady(CefRefPtr<CefRequestContext> context) {
  for (const char* name : {"autofill.profile_enabled", "autofill.credit_card_enabled",
                           "credentials_enable_service", "profile.password_manager_enabled"}) {
    if (!context->CanSetPreference(name)) {
      continue;
    }
    auto off = CefValue::Create();
    off->SetBool(false);
    CefString error;
    context->SetPreference(name, off, error);
  }
}

void TabCreated(Session& session, const std::string& id) {
  InstallGuest(session, id);
}

void TabClosing(Session& session, const std::string& id) {
  auto& state = StateOf(session);
  state.guest_tabs.erase(id);
  if (auto entry = session.tabs.find(id); entry != session.tabs.end()) {
    CloseDevTools(session, id, entry->second);
  }
  state.observers.erase(id);
  for (auto call = state.protocol_calls.begin(); call != state.protocol_calls.end();) {
    if (call->second.tab == id) {
      auto respond = std::move(call->second.respond);
      call = state.protocol_calls.erase(call);
      respond(Error(410, "The tab closed"));
    } else {
      ++call;
    }
  }
}

void DesktopReset(Session& session) {
  auto& state = StateOf(session);
  for (auto& [name, overlay] : state.overlays) {
    overlay.controller->Destroy();
  }
  state.overlays.clear();
}

bool Run(Session& session, const std::string& command, CefRefPtr<CefDictionaryValue> args,
         Respond respond) {
  auto& state = StateOf(session);
  const auto id = args->GetString("id").ToString();
  if (command == "ask/answer") {
    auto value = args->GetValue("value");
    AnswerQuestion(session, args->GetInt("question"), value ? value->Copy() : nullptr);
    respond({});
    return true;
  }
  if (command == "shell/overlay") {
    // A part of the Desktop's page that stays above the tabs (the volume and
    // brightness display, the list of saved logins): its picture, drawn by the
    // page, where the page shows it. Without a picture, it goes away.
    const auto name = args->GetString("name").ToString();
    if (!GuestName(name)) {
      respond(Error(400, "The picture takes a name"));
      return true;
    }
    auto shown = state.overlays.find(name);
    if (!args->HasKey("png")) {
      if (shown != state.overlays.end()) {
        shown->second.controller->Destroy();
        state.overlays.erase(shown);
      }
      respond({});
      return true;
    }
    const CefRect bounds(args->GetInt("x"), args->GetInt("y"), args->GetInt("width"), args->GetInt("height"));
    const double scale = Number(args, "scale");
    const auto png = args->GetString("png").ToString();
    if (bounds.width <= 0 || bounds.height <= 0 || bounds.width > kMaxOverlaySize ||
        bounds.height > kMaxOverlaySize || !(scale >= 1 && scale <= 4) || png.empty() ||
        png.size() > kMaxOverlayPicture) {
      respond(Error(400, "The picture takes a PNG, its place and its scale"));
      return true;
    }
    const auto data = CefBase64Decode(png);
    auto image = CefImage::CreateImage();
    if (!data || !session.window ||
        !image->AddPNG(static_cast<float>(scale), data->GetRawData(), data->GetSize())) {
      respond(Error(400, "The picture is not a PNG"));
      return true;
    }
    if (shown == state.overlays.end()) {
      if (state.overlays.size() >= kMaxOverlays) {
        respond(Error(409, "Too many pictures above the tabs"));
        return true;
      }
      auto picture = CefLabelButton::CreateLabelButton(new OverlayPicture(session.weak_from_this(), name), "");
      picture->SetInkDropEnabled(false);
      picture->SetFocusable(false);
      picture->SetInsets(CefInsets(0, 0, 0, 0));
      picture->SetHorizontalAlignment(CEF_HORIZONTAL_ALIGNMENT_CENTER);
      // Added after the tabs' views, it is above them.
      auto controller = session.window->AddOverlayView(picture, CEF_DOCKING_MODE_CUSTOM, false);
      shown = state.overlays.emplace(name, Overlay{controller, picture}).first;
    }
    shown->second.picture->SetImage(CEF_BUTTON_STATE_NORMAL, image);
    shown->second.controller->SetBounds(bounds);
    shown->second.controller->SetVisible(true);
    respond({});
    return true;
  }
  if (command == "browser/guest-script") {
    // Once per Desktop page: the password manager's script, for every tab.
    const auto source = args->GetString("source").ToString();
    const auto world = args->GetString("world").ToString();
    const auto binding = args->GetString("binding").ToString();
    const auto receiver = args->GetString("receiver").ToString();
    if (source.empty() || source.size() > kMaxGuestScript || !GuestName(world) || !GuestName(binding) ||
        !GuestName(receiver)) {
      respond(Error(400, "The script takes its source and the names of its world, binding and receiver"));
      return true;
    }
    if (!state.guest_script.empty() &&
        (state.guest_script != source || state.guest_world != world || state.guest_binding != binding)) {
      // The tabs keep the script they have; new tabs get this one.
      state.guest_tabs.clear();
    }
    state.guest_script = source;
    state.guest_world = world;
    state.guest_binding = binding;
    state.guest_receiver = receiver;
    for (const auto& [id, tab] : session.tabs) {
      InstallGuest(session, id);
    }
    respond({});
    return true;
  }
  if (command == "shell/idle") {
    const auto seconds = IdleSeconds();
    if (seconds < 0) {
      respond(Error(404, "The display does not say how long it has been idle"));
      return true;
    }
    auto result = CefDictionaryValue::Create();
    result->SetDouble("seconds", seconds);
    respond(Result(Dictionary(result)));
    return true;
  }
  if (command == "browser/download") {
    const auto wanted = args->GetString("download").ToString();
    const auto action = args->GetString("action").ToString();
    for (auto& [key, download] : state.downloads) {
      if (download.id != wanted || !download.callback) {
        continue;
      }
      if (action == "cancel") {
        download.callback->Cancel();
      } else if (action == "pause") {
        download.callback->Pause();
      } else if (action == "resume") {
        download.callback->Resume();
      } else {
        respond(Error(400, "Unknown download action"));
        return true;
      }
      respond({});
      return true;
    }
    respond(Error(404, "No such download"));
    return true;
  }
  if (command == "browser/certificate") {
    const auto op = args->GetString("op").ToString();
    const auto host = args->GetString("host").ToString();
    if (host.empty() || host.size() > 300) {
      respond(Error(400, "Name a host"));
      return true;
    }
    const auto refused = state.refused.find(host);
    if (op == "refused") {
      if (refused == state.refused.end()) {
        respond(Result(Null()));
        return true;
      }
      auto result = CefDictionaryValue::Create();
      result->SetString("issuer", refused->second.issuer);
      result->SetString("subject", refused->second.subject);
      result->SetDouble("validStart", refused->second.valid_start);
      result->SetDouble("validExpiry", refused->second.valid_expiry);
      respond(Result(Dictionary(result)));
    } else if (op == "allow") {
      if (refused == state.refused.end()) {
        respond(Result(Bool(false)));
        return true;
      }
      state.exceptions.insert(host + " " + refused->second.fingerprint);
      state.refused.erase(refused);
      respond(Result(Bool(true)));
    } else if (op == "allowed" || op == "forget") {
      const auto prefix = host + " ";
      bool found = false;
      for (auto exception = state.exceptions.begin(); exception != state.exceptions.end();) {
        if (exception->compare(0, prefix.size(), prefix) == 0) {
          found = true;
          exception = op == "forget" ? state.exceptions.erase(exception) : std::next(exception);
        } else {
          ++exception;
        }
      }
      if (op == "forget") {
        // Chromium keeps the decisions it was given until the contexts forget them.
        for (auto context : {session.browser_context, session.private_context}) {
          if (context) {
            context->ClearCertificateExceptions(nullptr);
          }
        }
        found = true;
      }
      respond(Result(Bool(found)));
    } else {
      respond(Error(400, "Unknown certificate operation"));
    }
    return true;
  }
  if (command == "browser/clear-data") {
    auto kinds = args->GetList("kinds");
    for (std::size_t index = 0; kinds && index < kinds->GetSize(); ++index) {
      const auto kind = kinds->GetString(index).ToString();
      if (kind == "cookies" && session.browser_context) {
        session.browser_context->GetCookieManager(nullptr)->DeleteCookies("", "", nullptr);
      } else if (kind == "cache") {
        if (Tab* tab = AnyTab(session, false)) {
          tab->browser->GetHost()->ExecuteDevToolsMethod(0, "Network.clearBrowserCache", nullptr);
        }
      }
    }
    respond(Result(Bool(true)));
    return true;
  }
  if (command == "browser/favicon") {
    std::vector<std::string> urls;
    auto list = args->GetList("urls");
    for (std::size_t index = 0; list && index < list->GetSize() && urls.size() < kMaxFaviconUrls; ++index) {
      const auto url = list->GetString(index).ToString();
      CefURLParts parts;
      const auto scheme = CefParseURL(url, parts) ? CefString(&parts.scheme).ToString() : "";
      if (scheme == "http" || scheme == "https" || scheme == "data") {
        urls.push_back(url);
      }
    }
    Tab* tab = AnyTab(session, args->GetBool("private"));
    if (!tab || urls.empty()) {
      auto result = CefDictionaryValue::Create();
      result->SetNull("url");
      respond(Result(Dictionary(result)));
      return true;
    }
    CefRefPtr<FaviconFetch> fetch = new FaviconFetch(tab->browser, std::move(urls), std::move(respond));
    fetch->Next();
    return true;
  }
  // The Desktop's own page, for the agent's screenshots and its hands on the desktop.
  if (command == "shell/protocol") {
    const auto method = args->GetString("method").ToString();
    if (!kShellProtocolMethods.count(method)) {
      respond(Error(403, "The method is not offered to the Desktop"));
      return true;
    }
    auto params = args->GetType("params") == VTYPE_DICTIONARY ? args->GetDictionary("params") : nullptr;
    auto pending = respond;
    if (!CallProtocol(session, std::string(), session.desktop, method, params, std::move(respond))) {
      pending(Error(409, "The Desktop page is not ready"));
    }
    return true;
  }
  // The commands below act on one tab.
  const bool tab_command = command == "browser/protocol" || command == "browser/devtools" ||
                           command == "browser/devtools-place" || command == "browser/audio" ||
                           command == "browser/pdf" || command == "browser/guest-send";
  if (!tab_command) {
    return false;
  }
  Tab* tab = FindTab(session, id);
  if (!tab) {
    respond(Error(404, "No such tab"));
    return true;
  }
  if (command == "browser/guest-send") {
    // To the password manager's script in the tab's top frame: in its world, through its receiver.
    const auto channel = args->GetString("channel").ToString();
    auto payload = args->GetValue("payload");
    if ((channel != "fill" && channel != "popup") || !payload || state.guest_receiver.empty()) {
      respond(Error(400, "The script takes a fill or the state of the list of logins"));
      return true;
    }
    auto channel_value = CefValue::Create();
    channel_value->SetString(channel);
    const auto expression = "globalThis[" + CefWriteJSON(StringValue(state.guest_receiver), JSON_WRITER_DEFAULT).ToString() +
                            "]?.(" + CefWriteJSON(channel_value, JSON_WRITER_DEFAULT).ToString() + ", " +
                            CefWriteJSON(payload, JSON_WRITER_DEFAULT).ToString() + ")";
    auto browser = tab->browser;
    const auto world = state.guest_world;
    std::weak_ptr<Session> weak = session.weak_from_this();
    ProtocolThen(session, id, browser, "Page.getFrameTree", nullptr, respond,
                 [weak, id, browser, world, expression, respond](CefRefPtr<CefDictionaryValue> tree) {
                   auto session = weak.lock();
                   auto root = tree->GetDictionary("frameTree");
                   auto frame = root ? root->GetDictionary("frame") : nullptr;
                   if (!session || !frame) {
                     respond(Error(409, "The tab has no page"));
                     return;
                   }
                   auto params = CefDictionaryValue::Create();
                   params->SetString("frameId", frame->GetString("id"));
                   params->SetString("worldName", world);
                   ProtocolThen(*session, id, browser, "Page.createIsolatedWorld", params, respond,
                                [weak, id, browser, expression, respond](CefRefPtr<CefDictionaryValue> created) {
                                  auto session = weak.lock();
                                  if (!session) {
                                    return;
                                  }
                                  auto call = CefDictionaryValue::Create();
                                  call->SetString("expression", expression);
                                  call->SetInt("contextId", created->GetInt("executionContextId"));
                                  ProtocolThen(*session, id, browser, "Runtime.evaluate", call, respond,
                                               [respond](CefRefPtr<CefDictionaryValue>) { respond({}); });
                                });
                 });
    return true;
  }
  if (command == "browser/protocol") {
    const auto method = args->GetString("method").ToString();
    if (!kProtocolMethods.count(method)) {
      respond(Error(403, "The method is not offered to the Desktop"));
      return true;
    }
    auto params = args->GetType("params") == VTYPE_DICTIONARY ? args->GetDictionary("params") : nullptr;
    auto pending = respond;
    if (!CallProtocol(session, id, tab->browser, method, params, std::move(respond))) {
      pending(Error(409, "The tab is still opening"));
    }
    return true;
  }
  if (command == "browser/devtools") {
    auto result = CefDictionaryValue::Create();
    if (args->GetBool("close") || (tab->devtools && args->GetBool("toggle"))) {
      CloseDevTools(session, id, *tab);
    } else {
      if (!tab->browser) {
        respond(Error(409, "The tab is still opening"));
        return true;
      }
      const std::string action = args->GetString("panel") == "console" ? "console" :
          args->GetString("mode") == "inspect" ? "inspect" : "";
      if (!tab->devtools) {
        tab->browser->GetHost()->ShowDevTools(CefWindowInfo(),
            new DevToolsClient(session.weak_from_this(), id, action), CefBrowserSettings(), CefPoint());
        if (!tab->devtools) {
          respond(Error(500, "Could not attach developer tools to the Browser"));
          return true;
        }
        // Chromium can show the new window before the Desktop has measured the dock.
        tab->devtools_window->Hide();
        tab->devtools_focus = true;
      } else if (auto frontend = tab->devtools->GetBrowser()) {
        DevToolsClient::RunAction(frontend->GetMainFrame(), action);
        if (tab->devtools_window->IsVisible()) {
          tab->devtools_window->Activate();
          tab->devtools->RequestFocus();
        }
      }
    }
    result->SetBool("open", tab->devtools != nullptr);
    respond(Result(Dictionary(result)));
    return true;
  }
  if (command == "browser/devtools-place") {
    if (tab->devtools_window && session.window) {
      auto bounds = CefRect(args->GetInt("x"), args->GetInt("y"), args->GetInt("width"), args->GetInt("height"));
      const bool shown = args->GetBool("visible") && bounds.width > 0 && bounds.height > 0;
      if (shown) {
        const auto origin = session.window->GetClientAreaBoundsInScreen();
        bounds.x += origin.x;
        bounds.y += origin.y;
        tab->devtools_window->SetBounds(bounds);
        if (!tab->devtools_window->IsVisible()) {
          tab->devtools_window->Show();
        }
        if (std::exchange(tab->devtools_focus, false)) {
          tab->devtools_window->Activate();
          tab->devtools->RequestFocus();
        }
      } else {
        tab->devtools_window->Hide();
      }
    }
    respond({});
    return true;
  }
  if (command == "browser/audio") {
    if (!tab->browser) {
      respond(Error(409, "The tab is still opening"));
      return true;
    }
    if (args->GetType("muted") == VTYPE_BOOL) {
      tab->browser->GetHost()->SetAudioMuted(args->GetBool("muted"));
    }
    auto result = CefDictionaryValue::Create();
    result->SetBool("muted", tab->browser->GetHost()->IsAudioMuted());
    respond(Result(Dictionary(result)));
    return true;
  }
  // browser/pdf: the page as a PDF file, which the Desktop prints.
  if (!tab->browser) {
    respond(Error(409, "The tab is still opening"));
    return true;
  }
  std::error_code error;
  const auto folder = std::filesystem::temp_directory_path(error) / "openshell-print";
  std::filesystem::create_directories(folder, error);
  if (error) {
    respond(Error(500, "No folder for the printout"));
    return true;
  }
  const auto path = folder / ("page-" + std::to_string(++state.pdf_counter) + ".pdf");
  tab->browser->GetHost()->PrintToPDF(path.string(), CefPdfPrintSettings(), new PdfDone(std::move(respond)));
  return true;
}

}  // namespace openshell::features
