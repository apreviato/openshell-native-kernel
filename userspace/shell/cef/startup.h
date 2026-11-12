#pragma once

#include <cstddef>
#include <cstdint>

struct OpenshellCefStartup;
struct OpenshellCefControl;

struct OpenshellCefStartupView {
  bool desktop;
  const char* primary_url;
  const char* profile;
  const char* guest_url;
};

enum OpenshellCefContext : std::uint32_t {
  OPEN_SHELL_PRIMARY = 0,
  OPEN_SHELL_GUEST = 1,
  OPEN_SHELL_BROWSER = 2,
};

enum OpenshellCefOperation : std::uint32_t {
  OPEN_SHELL_MAIN_FRAME = 0,
  OPEN_SHELL_SUBFRAME = 1,
  OPEN_SHELL_RESOURCE = 2,
};

extern "C" {
OpenshellCefStartup* openshell_cef_startup(OpenshellCefStartupView* view);
void openshell_cef_free(OpenshellCefStartup* startup);
OpenshellCefControl* openshell_cef_control(const OpenshellCefStartup* startup);
std::ptrdiff_t openshell_cef_control_next(const OpenshellCefControl* control,
                                          char* buffer, std::size_t capacity);
void openshell_cef_control_free(OpenshellCefControl* control);
std::uint16_t openshell_cef_hostd_port(const OpenshellCefStartup* startup);
bool openshell_cef_allows(const OpenshellCefStartup* startup, std::uint32_t context,
                         std::uint32_t operation, const char* url,
                         std::size_t length);
}
