#pragma once

#include <cstddef>
#include <cstdint>

struct OpenshellCefStartup;

struct OpenshellCefStartupView {
  bool desktop;
  const char* primary_url;
  const char* profile;
  const char* guest_url;
};

enum OpenshellCefOperation : std::uint32_t {
  OPEN_SHELL_MAIN_FRAME = 0,
  OPEN_SHELL_SUBFRAME = 1,
  OPEN_SHELL_RESOURCE = 2,
};

extern "C" {
OpenshellCefStartup* openshell_cef_startup(OpenshellCefStartupView* view);
void openshell_cef_free(OpenshellCefStartup* startup);
bool openshell_cef_allows(const OpenshellCefStartup* startup, bool guest,
                         std::uint32_t operation, const char* url,
                         std::size_t length);
}
