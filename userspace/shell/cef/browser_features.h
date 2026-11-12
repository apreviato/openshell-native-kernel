// The Browser features of the native Desktop that Electron's main process
// provides on OpenShell Live (apps/desktop/electron/main.ts): downloads with
// their progress, the questions the Desktop answers (camera and microphone,
// files to upload), certificate exceptions, DevTools docked in the Browser
// window, the DevTools protocol for the agent's tools, muting, favicons and
// clearing the Browser's data.
//
// main.cc calls these from its handlers and hands the commands of the Desktop
// page to Run. Everything runs on the browser process UI thread.
#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <vector>

#include "include/cef_dialog_handler.h"
#include "include/cef_download_handler.h"
#include "include/cef_permission_handler.h"
#include "include/cef_request_handler.h"
#include "session.h"

namespace openshell::features {

std::shared_ptr<State> CreateState();

// A context of the Browser's tabs is ready: Chromium's own autofill and
// password manager stay off. The Desktop's password manager fills logins,
// as on Live, where Electron has neither.
void BrowserContextReady(CefRefPtr<CefRequestContext> context);

// A tab's download goes to ~/Downloads; the Desktop follows its progress and
// can cancel, pause and resume it.
bool BeforeDownload(Session& session, const std::string& tab, CefRefPtr<CefDownloadItem> item,
                    const CefString& suggested_name, CefRefPtr<CefBeforeDownloadCallback> callback);
void DownloadUpdated(Session& session, CefRefPtr<CefDownloadItem> item,
                     CefRefPtr<CefDownloadItemCallback> callback);

// A tab asks for the camera or the microphone: the Desktop asks the person.
bool MediaAccess(Session& session, const std::string& tab, const CefString& origin,
                 uint32_t permissions, CefRefPtr<CefMediaAccessCallback> callback);
// Other permissions of a tab (location, notifications...) are refused, as on Live.
bool PermissionPrompt(Session& session, const std::string& tab, uint32_t permissions,
                      CefRefPtr<CefPermissionPromptCallback> callback);
// A page chooses files with the Desktop's picker. `page` is empty for the Desktop's own page.
bool FileDialog(Session& session, const CefString& page, CefDialogHandler::FileDialogMode mode,
                const std::vector<CefString>& accept, CefRefPtr<CefFileDialogCallback> callback);

// A tab met a certificate it does not trust: refused unless the person allowed it.
bool CertificateError(Session& session, const CefString& url, CefRefPtr<CefSSLInfo> ssl,
                      CefRefPtr<CefCallback> callback);

// The DevTools of a tab became a view: docked where the Desktop page places it.
bool DevToolsCreated(Session& session, const std::string& tab, CefRefPtr<CefBrowserView> devtools);
// Something was copied in any program: the Desktop keeps it in its clipboard history.
void ClipboardCopied(Session& session, const ClipboardCopy& copy);

// The tab's page exists: the password manager's script goes into it.
void TabCreated(Session& session, const std::string& tab);
// The tab closes: its DevTools and its protocol requests go with it.
void TabClosing(Session& session, const std::string& tab);
// A new Desktop page: what the previous one showed above the tabs goes away.
void DesktopReset(Session& session);

// Runs the command when it is one of these features. `respond` may be called
// later, once the command completes.
bool Run(Session& session, const std::string& command, CefRefPtr<CefDictionaryValue> args,
         Respond respond);

}  // namespace openshell::features
