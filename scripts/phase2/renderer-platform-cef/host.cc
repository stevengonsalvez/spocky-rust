#include "host.h"

#include "include/base/cef_callback.h"
#include "include/wrapper/cef_closure_task.h"

namespace {

constexpr int kWidth = 1280;
constexpr int kHeight = 800;

class HostClient : public CefClient, public CefLifeSpanHandler {
 public:
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }

  void OnAfterCreated(CefRefPtr<CefBrowser>) override { browser_count_++; }
  bool DoClose(CefRefPtr<CefBrowser>) override { return false; }
  void OnBeforeClose(CefRefPtr<CefBrowser>) override {
    if (--browser_count_ == 0) CefQuitMessageLoop();
  }

 private:
  int browser_count_ = 0;
  IMPLEMENT_REFCOUNTING(HostClient);
};

class HostBrowserViewDelegate : public CefBrowserViewDelegate {
 public:
  // Alloy style embeds only the page: no tab strip, toolbar, or omnibox.
  cef_runtime_style_t GetBrowserRuntimeStyle() override { return CEF_RUNTIME_STYLE_ALLOY; }

 private:
  IMPLEMENT_REFCOUNTING(HostBrowserViewDelegate);
};

class HostWindowDelegate : public CefWindowDelegate {
 public:
  explicit HostWindowDelegate(CefRefPtr<CefBrowserView> browser_view)
      : browser_view_(browser_view) {}

  void OnWindowCreated(CefRefPtr<CefWindow> window) override {
    window->AddChildView(browser_view_);
    window->Show();
    browser_view_->RequestFocus();
  }
  void OnWindowDestroyed(CefRefPtr<CefWindow>) override { browser_view_ = nullptr; }
  bool CanClose(CefRefPtr<CefWindow>) override {
    CefRefPtr<CefBrowser> browser = browser_view_->GetBrowser();
    return browser ? browser->GetHost()->TryCloseBrowser() : true;
  }
  bool IsFrameless(CefRefPtr<CefWindow>) override { return true; }
  bool CanResize(CefRefPtr<CefWindow>) override { return false; }
  CefRect GetInitialBounds(CefRefPtr<CefWindow>) override { return CefRect(0, 0, kWidth, kHeight); }
  CefSize GetPreferredSize(CefRefPtr<CefView>) override { return CefSize(kWidth, kHeight); }
  cef_runtime_style_t GetWindowRuntimeStyle() override { return CEF_RUNTIME_STYLE_ALLOY; }

 private:
  CefRefPtr<CefBrowserView> browser_view_;
  IMPLEMENT_REFCOUNTING(HostWindowDelegate);
  DISALLOW_COPY_AND_ASSIGN(HostWindowDelegate);
};

}  // namespace

void HostApp::OnContextInitialized() {
  CEF_REQUIRE_UI_THREAD();
  CefBrowserSettings browser_settings;
  browser_settings.background_color = CefColorSetARGB(255, 255, 255, 255);
  CefRefPtr<CefBrowserView> browser_view = CefBrowserView::CreateBrowserView(
      new HostClient, options_.url, browser_settings, nullptr, nullptr,
      new HostBrowserViewDelegate);
  CefWindow::CreateTopLevelWindow(new HostWindowDelegate(browser_view));
  CefPostDelayedTask(TID_UI, base::BindOnce(&CefQuitMessageLoop), options_.bound_ms);
}
