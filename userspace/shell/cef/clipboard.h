// The system clipboard for the Desktop's clipboard history (Super+V), as
// Electron's main process follows it on OpenShell Live: each new copy of any
// program, text or a PNG picture, with the formats it was offered in, so the
// Desktop can skip what password managers mark as secret.
//
// The watcher reads the X11 CLIPBOARD selection on its own connection and
// thread; XFixes says when its owner changes, so it never polls. What it read
// is handed to `deliver` on that thread.
#pragma once

#include <functional>
#include <memory>
#include <string>
#include <thread>
#include <vector>

namespace openshell {

struct ClipboardCopy {
  // "text" or "image" (PNG bytes in `data`).
  std::string kind;
  std::string data;
  // Every format the owner offered, by name: "UTF8_STRING", "image/png"...
  std::vector<std::string> formats;
};

class ClipboardWatcher {
 public:
  using Deliver = std::function<void(ClipboardCopy)>;

  // Null when there is no X display or no XFixes.
  static std::unique_ptr<ClipboardWatcher> Start(Deliver deliver);
  ~ClipboardWatcher();

  ClipboardWatcher(const ClipboardWatcher&) = delete;
  ClipboardWatcher& operator=(const ClipboardWatcher&) = delete;

 private:
  struct Connection;
  ClipboardWatcher(std::unique_ptr<Connection> connection, Deliver deliver);
  void Run();

  std::unique_ptr<Connection> connection_;
  Deliver deliver_;
  int wake_[2] = {-1, -1};
  std::thread thread_;
};

}  // namespace openshell
