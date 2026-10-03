// macOS entry point, modelled on cefsimple: an NSApplication that implements
// CefAppProtocol, the CEF framework loaded at runtime, and the CEF message loop.
#import <Cocoa/Cocoa.h>

#include <cstdlib>
#include <string>

#include "include/cef_application_mac.h"
#include "include/wrapper/cef_helpers.h"
#include "include/wrapper/cef_library_loader.h"

#include "host.h"

@interface HostApplication : NSApplication <CefAppProtocol> {
 @private
  BOOL handlingSendEvent_;
}
@end

@implementation HostApplication
- (BOOL)isHandlingSendEvent {
  return handlingSendEvent_;
}
- (void)setHandlingSendEvent:(BOOL)handlingSendEvent {
  handlingSendEvent_ = handlingSendEvent;
}
- (void)sendEvent:(NSEvent*)event {
  CefScopedSendingEvent sendingEventScoper;
  [super sendEvent:event];
}
- (void)terminate:(id)sender {
  CefQuitMessageLoop();
}
@end

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
  CefScopedLibraryLoader library_loader;
  if (!library_loader.LoadInMain()) return 1;

  const std::string port = ArgValue(argc, argv, "spocky-cdp-port");
  const std::string cache = ArgValue(argc, argv, "spocky-cache");
  if (port.empty() || port == "6767" || cache.empty()) {
    fprintf(stderr, "usage: host --spocky-cdp-port=PORT (not 6767) --spocky-cache=DIR [--spocky-url=URL] [--spocky-bound-ms=N]\n");
    return 2;
  }
  HostOptions options;
  if (!ArgValue(argc, argv, "spocky-url").empty()) options.url = ArgValue(argc, argv, "spocky-url");
  if (!ArgValue(argc, argv, "spocky-bound-ms").empty()) options.bound_ms = std::stoi(ArgValue(argc, argv, "spocky-bound-ms"));

  @autoreleasepool {
    [HostApplication sharedApplication];
    CefMainArgs main_args(argc, argv);
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
  }
  return 0;
}
