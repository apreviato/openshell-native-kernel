#include "session.h"

#include "include/cef_frame.h"
#include "include/cef_parser.h"

namespace openshell {

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

Reply Error(int status, const char* message) {
  auto error = CefDictionaryValue::Create();
  error->SetString("error", message);
  auto value = CefValue::Create();
  value->SetDictionary(error);
  return {status, CefWriteJSON(value, JSON_WRITER_DEFAULT).ToString()};
}

Reply Result(CefRefPtr<CefValue> value) {
  return {200, CefWriteJSON(value, JSON_WRITER_DEFAULT).ToString()};
}

std::string DownloadName(std::string name) {
  name = std::filesystem::path(name).filename().string();
  std::string clean;
  for (unsigned char c : name) {
    if (c >= 0x20 && c != 0x7f && c != '/' && c != '\\') {
      clean += static_cast<char>(c);
    }
  }
  while (!clean.empty() && (clean.front() == '.' || clean.front() == ' ')) {
    clean.erase(clean.begin());
  }
  if (clean.size() > 200) {
    clean.resize(200);
  }
  return clean.empty() ? "download" : clean;
}

std::filesystem::path FreeDownloadPath(const std::filesystem::path& folder, const std::string& name) {
  const auto stem = std::filesystem::path(name).stem().string();
  const auto extension = std::filesystem::path(name).extension().string();
  auto candidate = folder / name;
  for (int copy = 1; std::filesystem::exists(candidate) && copy < 1000; ++copy) {
    candidate = folder / (stem + " (" + std::to_string(copy) + ")" + extension);
  }
  return candidate;
}

}  // namespace openshell
