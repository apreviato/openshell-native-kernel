#include "clipboard.h"

#include <chrono>
#include <cstring>

#include <X11/Xatom.h>
#include <X11/Xlib.h>
#include <X11/extensions/Xfixes.h>
#include <poll.h>
#include <unistd.h>

namespace openshell {
namespace {

// What a copy may weigh: larger ones stay out of the history.
constexpr std::size_t kMaxText = 1 << 20;
constexpr std::size_t kMaxImage = 16 << 20;
// How long the owner of the selection has to answer.
constexpr auto kAnswerTimeout = std::chrono::seconds(2);

}  // namespace

struct ClipboardWatcher::Connection {
  Display* display = nullptr;
  Window window = 0;
  int xfixes_event = 0;
  Atom clipboard = 0, targets = 0, utf8 = 0, png = 0, incr = 0, property = 0;

  ~Connection() {
    if (display) {
      if (window) {
        XDestroyWindow(display, window);
      }
      XCloseDisplay(display);
    }
  }
};

std::unique_ptr<ClipboardWatcher> ClipboardWatcher::Start(Deliver deliver) {
  auto connection = std::make_unique<Connection>();
  connection->display = XOpenDisplay(nullptr);
  if (!connection->display) {
    return nullptr;
  }
  int error = 0;
  if (!XFixesQueryExtension(connection->display, &connection->xfixes_event, &error)) {
    return nullptr;
  }
  Display* display = connection->display;
  connection->window = XCreateSimpleWindow(display, DefaultRootWindow(display), 0, 0, 1, 1, 0, 0, 0);
  XSelectInput(display, connection->window, PropertyChangeMask);
  connection->clipboard = XInternAtom(display, "CLIPBOARD", False);
  connection->targets = XInternAtom(display, "TARGETS", False);
  connection->utf8 = XInternAtom(display, "UTF8_STRING", False);
  connection->png = XInternAtom(display, "image/png", False);
  connection->incr = XInternAtom(display, "INCR", False);
  connection->property = XInternAtom(display, "OPENSHELL_CLIPBOARD", False);
  XFixesSelectSelectionInput(display, connection->window, connection->clipboard,
                             XFixesSetSelectionOwnerNotifyMask);
  XFlush(display);
  return std::unique_ptr<ClipboardWatcher>(new ClipboardWatcher(std::move(connection), std::move(deliver)));
}

ClipboardWatcher::ClipboardWatcher(std::unique_ptr<Connection> connection, Deliver deliver)
    : connection_(std::move(connection)), deliver_(std::move(deliver)) {
  if (pipe(wake_) != 0) {
    wake_[0] = wake_[1] = -1;
  }
  thread_ = std::thread(&ClipboardWatcher::Run, this);
}

ClipboardWatcher::~ClipboardWatcher() {
  if (wake_[1] >= 0) {
    const char stop = 0;
    (void)!write(wake_[1], &stop, 1);
  }
  if (thread_.joinable()) {
    thread_.join();
  }
  for (int fd : wake_) {
    if (fd >= 0) {
      close(fd);
    }
  }
}

namespace {

// Waits for an event of `type` on `window`, or for the stop pipe. False on timeout or stop.
bool WaitFor(Display* display, int stop, int type, Window window, XEvent& event,
             std::chrono::steady_clock::time_point deadline) {
  for (;;) {
    while (XPending(display)) {
      XNextEvent(display, &event);
      if (event.type == type && event.xany.window == window) {
        return true;
      }
    }
    const auto left = std::chrono::duration_cast<std::chrono::milliseconds>(deadline - std::chrono::steady_clock::now());
    if (left.count() <= 0) {
      return false;
    }
    pollfd fds[2] = {{ConnectionNumber(display), POLLIN, 0}, {stop, POLLIN, 0}};
    if (poll(fds, stop >= 0 ? 2 : 1, static_cast<int>(left.count())) < 0 || (fds[1].revents & POLLIN)) {
      return false;
    }
  }
}

}  // namespace

void ClipboardWatcher::Run() {
  Connection& x = *connection_;
  Display* display = x.display;
  const int stop = wake_[0];

  // Asks the owner for `target` and reads the answer, large ones in pieces (INCR).
  auto read = [&](Atom target, std::size_t limit, Atom& type) -> std::string {
    XDeleteProperty(display, x.window, x.property);
    XConvertSelection(display, x.clipboard, target, x.property, x.window, CurrentTime);
    XFlush(display);
    XEvent event;
    const auto deadline = std::chrono::steady_clock::now() + kAnswerTimeout;
    if (!WaitFor(display, stop, SelectionNotify, x.window, event, deadline) ||
        event.xselection.property == None) {
      return {};
    }
    auto take = [&](bool remove, Atom& got) -> std::string {
      Atom actual = None;
      int format = 0;
      unsigned long count = 0;
      unsigned long after = 0;
      unsigned char* data = nullptr;
      if (XGetWindowProperty(display, x.window, x.property, 0, static_cast<long>(limit / 4 + 1), remove,
                             AnyPropertyType, &actual, &format, &count, &after, &data) != Success) {
        return {};
      }
      got = actual;
      const std::size_t unit = format == 32 ? sizeof(long) : static_cast<std::size_t>(format / 8);
      std::string bytes(reinterpret_cast<char*>(data), data ? count * unit : 0);
      if (data) {
        XFree(data);
      }
      return after ? std::string() : bytes;
    };
    std::string value = take(true, type);
    if (type != x.incr) {
      return value.size() <= limit ? value : std::string();
    }
    // INCR: the owner writes the copy in pieces, each after the last was read.
    std::string whole;
    for (;;) {
      XEvent change;
      do {
        if (!WaitFor(display, stop, PropertyNotify, x.window, change, std::chrono::steady_clock::now() + kAnswerTimeout)) {
          return {};
        }
      } while (change.xproperty.atom != x.property || change.xproperty.state != PropertyNewValue);
      Atom piece_type = None;
      const auto piece = take(true, piece_type);
      if (piece.empty()) {
        type = piece_type;
        return whole;
      }
      whole += piece;
      if (whole.size() > limit) {
        return {};
      }
    }
  };

  for (;;) {
    XEvent event;
    pollfd fds[2] = {{ConnectionNumber(display), POLLIN, 0}, {stop, POLLIN, 0}};
    if (!XPending(display) && (poll(fds, stop >= 0 ? 2 : 1, -1) < 0 || (fds[1].revents & POLLIN))) {
      return;
    }
    while (XPending(display)) {
      XNextEvent(display, &event);
      if (event.type != x.xfixes_event + XFixesSelectionNotify) {
        continue;
      }
      const auto& notify = reinterpret_cast<const XFixesSelectionNotifyEvent&>(event);
      if (notify.owner == None) {
        continue;  // The owner went away: nothing new was copied.
      }
      Atom type = None;
      const auto list = read(x.targets, 64 * 1024, type);
      ClipboardCopy copy;
      bool has_png = false;
      bool has_text = false;
      for (std::size_t offset = 0; offset + sizeof(Atom) <= list.size(); offset += sizeof(Atom)) {
        Atom atom;
        std::memcpy(&atom, list.data() + offset, sizeof(Atom));
        if (char* name = XGetAtomName(display, atom)) {
          copy.formats.emplace_back(name);
          XFree(name);
        }
        has_png = has_png || atom == x.png;
        has_text = has_text || atom == x.utf8 || atom == XA_STRING;
      }
      if (has_png) {
        copy.kind = "image";
        copy.data = read(x.png, kMaxImage, type);
      } else if (has_text) {
        copy.kind = "text";
        copy.data = read(x.utf8, kMaxText, type);
      }
      if (!copy.kind.empty() && !copy.data.empty()) {
        deliver_(std::move(copy));
      }
    }
  }
}

}  // namespace openshell
