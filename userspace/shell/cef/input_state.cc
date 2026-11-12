#include "input_state.h"

#include <X11/Xlib.h>
#include <X11/extensions/scrnsaver.h>

namespace openshell {
namespace {

Display* Connection() {
  static Display* display = XOpenDisplay(nullptr);
  return display;
}

}  // namespace

double IdleSeconds() {
  static const bool available = [] {
    int event_base = 0;
    int error_base = 0;
    return Connection() && XScreenSaverQueryExtension(Connection(), &event_base, &error_base);
  }();
  if (!available) {
    return -1;
  }
  XScreenSaverInfo* info = XScreenSaverAllocInfo();
  if (!info) {
    return -1;
  }
  double seconds = -1;
  if (XScreenSaverQueryInfo(Connection(), DefaultRootWindow(Connection()), info)) {
    seconds = static_cast<double>(info->idle) / 1000.0;
  }
  XFree(info);
  return seconds;
}

bool PointerOnScreen(int& x, int& y) {
  Display* display = Connection();
  if (!display) {
    return false;
  }
  Window root = 0;
  Window child = 0;
  int window_x = 0;
  int window_y = 0;
  unsigned int mask = 0;
  return XQueryPointer(display, DefaultRootWindow(display), &root, &child, &x, &y, &window_x, &window_y, &mask);
}

}  // namespace openshell
