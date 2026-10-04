// Windows entry point: one process type per executable, like cefsimple. The browser
// process initializes CEF with the loopback DevTools port and runs the CEF loop.
#include <windows.h>

#include <cstdio>
#include <cstdlib>
#include <string>

#include "include/cef_app.h"
#include "include/wrapper/cef_helpers.h"

#include "host.h"

namespace {
std::string ArgValue(int argc, char* argv[], const std::string& name) {
  const std::string prefix = "--" + name + "=";
  for (int i = 1; i < argc; i++) {
    const std::string argument = argv[i];
    if (argument.rfind(prefix, 0) == 0) return argument.substr(prefix.size());
  }
  return "";
}
}  // namespace

int main(int argc, char* argv[]) {
  CefMainArgs main_args(GetModuleHandle(nullptr));
  // Renderer, GPU, and utility processes run this same executable.
  const int exit_code = CefExecuteProcess(main_args, nullptr, nullptr);
  if (exit_code >= 0) return exit_code;

  const std::string port = ArgValue(argc, argv, "spocky-cdp-port");
  const std::string cache = ArgValue(argc, argv, "spocky-cache");
  if (port.empty() || port == "6767" || cache.empty()) {
    fprintf(stderr, "usage: host --spocky-cdp-port=PORT (not 6767) --spocky-cache=DIR [--spocky-url=URL] [--spocky-bound-ms=N]\n");
    return 2;
  }
  HostOptions options;
  if (!ArgValue(argc, argv, "spocky-url").empty()) options.url = ArgValue(argc, argv, "spocky-url");
  if (!ArgValue(argc, argv, "spocky-bound-ms").empty()) options.bound_ms = std::stoi(ArgValue(argc, argv, "spocky-bound-ms"));

  CefSettings settings;
  settings.no_sandbox = true;
  settings.remote_debugging_port = std::stoi(port);
  CefString(&settings.root_cache_path) = cache;
  CefString(&settings.cache_path) = cache;
  CefString(&settings.locale) = "en-US";
  CefString(&settings.accept_language_list) = "en-US";
  CefRefPtr<HostApp> app(new HostApp(options));
  if (!CefInitialize(main_args, settings, app.get(), nullptr)) return 1;
  CefRunMessageLoop();
  CefShutdown();
  return 0;
}
