// Spocky CEF host: one frameless 1280x800 Views window with a single browser,
// no browser chrome, and a DevTools port for renderer-platform-cdp-capture.cjs.
#pragma once

#include <string>

#include "include/cef_app.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/views/cef_browser_view.h"
#include "include/views/cef_browser_view_delegate.h"
#include "include/views/cef_window.h"
#include "include/views/cef_window_delegate.h"
#include "include/wrapper/cef_helpers.h"

struct HostOptions {
  std::string url = "about:blank";
  int bound_ms = 600000;  // the host quits by itself so an orphan cannot linger
  bool use_popup = false;  // native popup window instead of a Views window
};

class HostApp : public CefApp, public CefBrowserProcessHandler {
 public:
  explicit HostApp(HostOptions options) : options_(options) {}

  CefRefPtr<CefBrowserProcessHandler> GetBrowserProcessHandler() override { return this; }
  void OnBeforeCommandLineProcessing(const CefString& process_type,
                                     CefRefPtr<CefCommandLine> command_line) override;
  void OnContextInitialized() override;

 private:
  HostOptions options_;
  IMPLEMENT_REFCOUNTING(HostApp);
  DISALLOW_COPY_AND_ASSIGN(HostApp);
};
