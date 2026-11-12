#include <cstdlib>
#include <iostream>
#include <memory>
#include <string>
#include <utility>

#include <unistd.h>

#include "include/base/cef_compiler_specific.h"
#include "include/cef_app.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/cef_command_line.h"
#include "include/cef_frame.h"
#include "include/cef_request.h"
#include "include/cef_request_context.h"
#include "include/cef_request_context_handler.h"
#include "include/cef_resource_request_handler.h"
#include "include/views/cef_box_layout.h"
#include "include/views/cef_browser_view.h"
#include "include/views/cef_fill_layout.h"
#include "include/views/cef_window.h"
#include "startup.h"

#ifndef CEF_USE_SANDBOX
#error OpenShell requires a sandbox-enabled CEF build
#endif

namespace {

constexpr char kTitle[] = "OpenShell Native";
using Startup = std::shared_ptr<OpenshellCefStartup>;

bool Allows(const Startup& startup, bool guest, OpenshellCefOperation operation,
            const CefString& url) {
  const auto value = url.ToString();
  return openshell_cef_allows(startup.get(), guest, operation, value.data(), value.size());
}

struct Session {
  int browsers = 0;
  bool failed = false;
};

class RequestContextHandler final : public CefRequestContextHandler,
                                    public CefResourceRequestHandler {
 public:
  RequestContextHandler(Startup startup, bool guest)
      : startup_(std::move(startup)), guest_(guest) {}

  CefRefPtr<CefResourceRequestHandler> GetResourceRequestHandler(
      CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, CefRefPtr<CefRequest>,
      bool, bool, const CefString&, bool&) override {
    return this;
  }

  ReturnValue OnBeforeResourceLoad(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>,
                                   CefRefPtr<CefRequest> request,
                                   CefRefPtr<CefCallback>) override {
    return Allows(startup_, guest_, OPEN_SHELL_RESOURCE, request->GetURL())
               ? RV_CONTINUE
               : RV_CANCEL;
  }

  void OnResourceRedirect(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>,
                          CefRefPtr<CefRequest>, CefRefPtr<CefResponse>,
                          CefString& target) override {
    if (!Allows(startup_, guest_, OPEN_SHELL_RESOURCE, target)) {
      target = "about:blank";
    }
  }

 private:
  const Startup startup_;
  const bool guest_;
  IMPLEMENT_REFCOUNTING(RequestContextHandler);
};

class BrowserClient final : public CefClient,
                            public CefDisplayHandler,
                            public CefLifeSpanHandler,
                            public CefLoadHandler,
                            public CefRequestHandler {
 public:
  BrowserClient(Startup startup, bool guest, bool desktop,
                std::shared_ptr<Session> session)
      : startup_(std::move(startup)), guest_(guest), desktop_(desktop),
        session_(std::move(session)) {}

  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }

  bool OnBeforeBrowse(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                      CefRefPtr<CefRequest> request, bool, bool) override {
    const auto operation = frame && !frame->IsMain()
                               ? OPEN_SHELL_SUBFRAME
                               : OPEN_SHELL_MAIN_FRAME;
    return !Allows(startup_, guest_, operation, request->GetURL());
  }

  bool OnOpenURLFromTab(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>,
                        const CefString&, cef_window_open_disposition_t, bool) override {
    return true;
  }

  bool OnBeforePopup(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, int,
                     const CefString&, const CefString&, cef_window_open_disposition_t,
                     bool, const CefPopupFeatures&, CefWindowInfo&,
                     CefRefPtr<CefClient>&, CefBrowserSettings&,
                     CefRefPtr<CefDictionaryValue>&, bool*) override {
    return true;
  }

  void OnLoadError(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                   ErrorCode error, const CefString&, const CefString&) override {
    if (frame && frame->IsMain() && error != ERR_ABORTED) {
      std::cerr << "CEF " << (guest_ ? "guest" : "desktop")
                << " navigation failed (" << error << ")\n";
    }
  }

  void OnRenderProcessTerminated(CefRefPtr<CefBrowser>, TerminationStatus status,
                                 int error_code,
                                 const CefString& error_string) override {
    std::cerr << "CEF " << (guest_ ? "guest" : "desktop")
              << " renderer terminated (status " << status << ", code "
              << error_code << ", " << error_string.ToString() << ")\n";
  }

  void OnTitleChange(CefRefPtr<CefBrowser> browser,
                     const CefString& title) override {
    if (!desktop_ && !guest_) {
      auto view = CefBrowserView::GetForBrowser(browser);
      if (view) {
        auto window = view->GetWindow();
        if (window) {
          window->SetTitle(title);
        }
      }
    }
  }

  void OnAfterCreated(CefRefPtr<CefBrowser>) override { ++session_->browsers; }

  void OnBeforeClose(CefRefPtr<CefBrowser>) override {
    if (--session_->browsers == 0) {
      CefQuitMessageLoop();
    }
  }

 private:
  const Startup startup_;
  const bool guest_;
  const bool desktop_;
  const std::shared_ptr<Session> session_;
  IMPLEMENT_REFCOUNTING(BrowserClient);
};

class WindowDelegate final : public CefWindowDelegate {
 public:
  WindowDelegate(CefRefPtr<CefBrowserView> primary,
                 CefRefPtr<CefBrowserView> guest, bool fullscreen)
      : primary_(primary), guest_(guest), fullscreen_(fullscreen) {}

  void OnWindowCreated(CefRefPtr<CefWindow> window) override {
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
      auto browser = guest_->GetBrowser();
      if (browser) {
        browser->GetHost()->CloseBrowser(true);
      }
    }
  }

  void OnWindowDestroyed(CefRefPtr<CefWindow>) override {
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
  IMPLEMENT_REFCOUNTING(WindowDelegate);
};

class ViewDelegate final : public CefBrowserViewDelegate {
 public:
  cef_runtime_style_t GetBrowserRuntimeStyle() override {
    return CEF_RUNTIME_STYLE_ALLOY;
  }

 private:
  IMPLEMENT_REFCOUNTING(ViewDelegate);
};

class DesktopApp final : public CefApp, public CefBrowserProcessHandler {
 public:
  DesktopApp(Startup startup, OpenshellCefStartupView config)
      : startup_(std::move(startup)), config_(config) {}

  CefRefPtr<CefBrowserProcessHandler> GetBrowserProcessHandler() override {
    return this;
  }

  void OnBeforeCommandLineProcessing(
      const CefString& type, CefRefPtr<CefCommandLine> command_line) override {
    if (type.empty()) {
      command_line->AppendSwitchWithValue("ozone-platform", "x11");
      // Verbose Chromium logs for the test fixtures. Child processes inherit
      // the switches. The Desktop never enables them.
      if (!config_.desktop && std::getenv("OPENSHELL_CEF_DIAGNOSTICS")) {
        command_line->AppendSwitchWithValue("enable-logging", "stderr");
        command_line->AppendSwitchWithValue("v", "1");
      }
    }
  }

  void OnContextInitialized() override {
    auto context = CreateContext(false);
    if (!context) {
      FailStartup();
      return;
    }
    CefRefPtr<CefBrowserView> guest;
    if (*config_.guest_url) {
      auto guest_context = CreateContext(true);
      if (!guest_context || guest_context->IsSharingWith(context) ||
          !(guest = CreateView(guest_context, true))) {
        FailStartup();
        return;
      }
    }
    auto primary = CreateView(context, false);
    if (!primary || !CefWindow::CreateTopLevelWindow(
                        new WindowDelegate(primary, guest, config_.desktop))) {
      FailStartup();
    }
  }

  bool failed() const { return session_->failed; }

 private:
  CefRefPtr<CefRequestContext> CreateContext(bool guest) const {
    CefRequestContextSettings settings;
    if (!guest && config_.desktop) {
      CefString(&settings.cache_path) = std::string(config_.profile) + "/desktop";
    }
    return CefRequestContext::CreateContext(
        settings, new RequestContextHandler(startup_, guest));
  }

  CefRefPtr<CefBrowserView> CreateView(CefRefPtr<CefRequestContext> context,
                                       bool guest) const {
    return CefBrowserView::CreateBrowserView(
        new BrowserClient(startup_, guest, config_.desktop, session_),
        guest ? config_.guest_url : config_.primary_url, CefBrowserSettings(),
        nullptr, context, new ViewDelegate);
  }

  void FailStartup() {
    session_->failed = true;
    std::cerr << "Unable to create the OpenShell CEF window and contexts\n";
    CefQuitMessageLoop();
  }

  const Startup startup_;
  const OpenshellCefStartupView config_;
  const std::shared_ptr<Session> session_ = std::make_shared<Session>();
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

  CefRunMessageLoop();
  const bool failed = app->failed();
  CefShutdown();
  return failed ? 1 : 0;
}
