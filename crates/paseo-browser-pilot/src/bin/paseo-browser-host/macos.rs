use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use paseo_browser_pilot::{BrowserPilot, DeepLinkDelivery, PilotConfig};
use paseo_ui_renderer_pilot::render_shell_html;
use serde_json::{Value, json};
use tao::{
    dpi::LogicalSize,
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy},
    platform::{
        macos::{ActivationPolicy, EventLoopExtMacOS},
        run_return::EventLoopExtRunReturn,
    },
    window::{Window, WindowBuilder},
};
use wry::{WebView, WebViewBuilder};

const HOST_ID: &str = "host-a";
const DOWNLOAD_BODY: &[u8] = b"paseo-download-ok!";

#[derive(Debug)]
enum HostEvent {
    PageMessage(String),
    Navigation { allowed: bool, url: String },
    DownloadStarted { url: String },
    DownloadCompleted { success: bool, bytes: u64 },
}

#[derive(Debug)]
struct Options {
    deep_link: String,
    checkpoint: Option<PathBuf>,
    fail_before_ready: bool,
}

struct HostProgress {
    started: Instant,
    failed: bool,
    milestones: BTreeSet<Milestone>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Milestone {
    NavigationDenied,
    NavigationAllowed,
    DeepLinkDelivered,
    DownloadCompleted,
}

pub fn run() -> ExitCode {
    let options = match Options::parse() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    match run_host(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            emit(json!({
                "operation": "host.failed",
                "reason": "runtime_error",
                "detail": message,
            }));
            ExitCode::FAILURE
        }
    }
}

fn run_host(options: &Options) -> Result<(), String> {
    let deep_link = resolve_deep_link(&options.deep_link)?;
    let fixture = FixtureServer::start(&deep_link)?;
    let root_url = fixture.url("/host-a/root");
    let download_path = std::env::temp_dir().join(format!(
        "paseo-browser-host-download-{}.txt",
        std::process::id()
    ));
    let resumed = options
        .checkpoint
        .as_ref()
        .is_some_and(|path| path.is_file());

    let mut event_loop = EventLoopBuilder::<HostEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    let window = WindowBuilder::new()
        .with_title("Paseo browser host pilot")
        .with_inner_size(LogicalSize::new(800.0, 600.0))
        .with_visible(false)
        .build(&event_loop)
        .map_err(|error| error.to_string())?;
    let _webview = build_webview(
        &window,
        &fixture,
        &root_url,
        &download_path,
        event_loop.create_proxy(),
    )?;

    emit(json!({
        "operation": "host.started",
        "engine": "WKWebView",
        "hostId": HOST_ID,
        "resumed": resumed,
    }));

    let mut progress = HostProgress::new();
    progress.run(&mut event_loop, options);

    let _ = fs::remove_file(download_path);
    if progress.failed {
        Err("browser host did not reach ready state".to_owned())
    } else {
        Ok(())
    }
}

fn build_webview(
    window: &Window,
    fixture: &FixtureServer,
    root_url: &str,
    download_path: &Path,
    proxy: EventLoopProxy<HostEvent>,
) -> Result<WebView, String> {
    let navigation_proxy = proxy.clone();
    let navigation_origin = fixture.origin();
    let ipc_proxy = proxy.clone();
    let download_started_proxy = proxy.clone();
    let configured_download_path = download_path.to_path_buf();
    let completed_download_path = download_path.to_path_buf();
    WebViewBuilder::new()
        .with_url(root_url)
        .with_navigation_handler(move |url| {
            let allowed = url.starts_with(&format!("{navigation_origin}/{HOST_ID}/"));
            let _ = navigation_proxy.send_event(HostEvent::Navigation {
                allowed,
                url: url.clone(),
            });
            allowed
        })
        .with_ipc_handler(move |request| {
            let _ = ipc_proxy.send_event(HostEvent::PageMessage(request.body().clone()));
        })
        .with_download_started_handler(move |url, destination| {
            destination.clone_from(&configured_download_path);
            let _ = download_started_proxy.send_event(HostEvent::DownloadStarted { url });
            true
        })
        .with_download_completed_handler(move |_url, _path, success| {
            let bytes = fs::metadata(&completed_download_path)
                .map(|metadata| metadata.len())
                .unwrap_or_default();
            let _ = proxy.send_event(HostEvent::DownloadCompleted { success, bytes });
        })
        .build(window)
        .map_err(|error| error.to_string())
}

impl HostProgress {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            failed: false,
            milestones: BTreeSet::new(),
        }
    }

    fn run(&mut self, event_loop: &mut EventLoop<HostEvent>, options: &Options) {
        event_loop.run_return(|event, _, control_flow| {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(100));
            match event {
                Event::UserEvent(event) => self.handle_event(event, options, control_flow),
                Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                    self.handle_timer(control_flow);
                }
                _ => {}
            }
        });
    }

    fn handle_event(
        &mut self,
        event: HostEvent,
        options: &Options,
        control_flow: &mut ControlFlow,
    ) {
        match event {
            HostEvent::Navigation { allowed, url } => self.handle_navigation(allowed, &url),
            HostEvent::PageMessage(body) => self.handle_page_message(&body, options, control_flow),
            HostEvent::DownloadStarted { url } => {
                emit(json!({ "operation": "download.started", "url": url }));
            }
            HostEvent::DownloadCompleted { success, bytes } => {
                if success && bytes == DOWNLOAD_BODY.len() as u64 {
                    self.milestones.insert(Milestone::DownloadCompleted);
                }
                emit(json!({
                    "operation": "download.completed",
                    "success": success,
                    "bytes": bytes,
                }));
            }
        }
    }

    fn handle_navigation(&mut self, allowed: bool, url: &str) {
        if allowed && url.contains("/host-a/next") {
            self.milestones.insert(Milestone::NavigationAllowed);
            emit(json!({ "operation": "navigation.allowed", "url": url }));
        } else if !allowed {
            self.milestones.insert(Milestone::NavigationDenied);
            emit(json!({ "operation": "navigation.denied", "url": url }));
        }
    }

    fn handle_page_message(
        &mut self,
        body: &str,
        options: &Options,
        control_flow: &mut ControlFlow,
    ) {
        let message: Value = match serde_json::from_str(body) {
            Ok(message) => message,
            Err(error) => {
                emit(json!({
                    "operation": "guest.message_invalid",
                    "detail": error.to_string(),
                }));
                return;
            }
        };
        match message["kind"].as_str() {
            Some("loaded") => self.handle_loaded(&message, options, control_flow),
            Some("deep_link") => {
                self.milestones.insert(Milestone::DeepLinkDelivered);
                emit(json!({
                    "operation": "deep_link.delivered",
                    "serverId": message["serverId"],
                    "agentId": message["agentId"],
                }));
            }
            _ => {}
        }
    }

    fn handle_loaded(
        &mut self,
        message: &Value,
        options: &Options,
        control_flow: &mut ControlFlow,
    ) {
        emit(json!({
            "operation": "guest.loaded",
            "hostId": HOST_ID,
            "page": message["page"],
            "uiRendered": message["uiRendered"],
        }));
        if options.fail_before_ready && message["page"] == "root" {
            if let Some(path) = &options.checkpoint
                && let Err(error) = write_checkpoint(path, &options.deep_link)
            {
                emit(json!({
                    "operation": "host.failed",
                    "reason": "checkpoint_write_failed",
                    "detail": error,
                }));
            }
            emit(json!({
                "operation": "host.failed",
                "reason": "injected_failure",
            }));
            self.failed = true;
            *control_flow = ControlFlow::Exit;
        }
    }

    fn handle_timer(&mut self, control_flow: &mut ControlFlow) {
        if self.milestones.len() == 4 {
            emit(json!({ "operation": "host.ready", "hostId": HOST_ID }));
            *control_flow = ControlFlow::Exit;
        } else if self.started.elapsed() >= Duration::from_secs(15) {
            emit(json!({
                "operation": "host.failed",
                "reason": "timeout",
            }));
            self.failed = true;
            *control_flow = ControlFlow::Exit;
        }
    }
}

fn resolve_deep_link(input: &str) -> Result<Value, String> {
    let mut pilot = BrowserPilot::new(PilotConfig::all_supported());
    let delivery = pilot
        .receive_deep_link(HOST_ID, input)
        .map_err(|error| error.message)?;
    if delivery != DeepLinkDelivery::Queued {
        return Err("deep link bypassed startup queue".to_owned());
    }
    let target = pilot
        .host_ready(HOST_ID)
        .ok_or_else(|| "queued deep link was not delivered".to_owned())?;
    serde_json::to_value(target).map_err(|error| error.to_string())
}

fn write_checkpoint(path: &Path, deep_link: &str) -> Result<(), String> {
    let bytes = serde_json::to_vec(&json!({
        "hostId": HOST_ID,
        "deepLink": deep_link,
    }))
    .map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| error.to_string())
}

fn emit(value: impl std::fmt::Display) {
    println!("{value}");
}

impl Options {
    fn parse() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        if args.next().as_deref() != Some("--self-test") {
            return Err("usage: paseo-browser-host --self-test --deep-link <url>".to_owned());
        }
        let mut deep_link = None;
        let mut checkpoint = None;
        let mut fail_before_ready = false;
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--deep-link" => deep_link = args.next(),
                "--checkpoint" => checkpoint = args.next().map(PathBuf::from),
                "--fail-before-ready" => fail_before_ready = true,
                _ => return Err(format!("unknown argument: {argument}")),
            }
        }
        let deep_link = match deep_link {
            Some(deep_link) => deep_link,
            None if checkpoint.is_some() => {
                read_checkpoint(checkpoint.as_deref().expect("checkpoint existence checked"))?
            }
            None => return Err("--deep-link is required".to_owned()),
        };
        Ok(Self {
            deep_link,
            checkpoint,
            fail_before_ready,
        })
    }
}

fn read_checkpoint(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("checkpoint read failed: {error}"))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("checkpoint parse failed: {error}"))?;
    value["deepLink"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "checkpoint has no deepLink".to_owned())
}

struct FixtureServer {
    address: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FixtureServer {
    fn start(deep_link: &Value) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let deep_link = deep_link.clone();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &deep_link),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            address: address.to_string(),
            stop,
            thread: Some(thread),
        })
    }

    fn origin(&self) -> String {
        format!("http://{}", self.address)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin())
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(mut stream: TcpStream, deep_link: &Value) {
    let mut request = [0_u8; 4096];
    let Ok(length) = stream.read(&mut request) else {
        return;
    };
    let request = String::from_utf8_lossy(&request[..length]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let (status, content_type, extra_headers, body) = match path {
        "/host-a/root" => (
            "200 OK",
            "text/html; charset=utf-8",
            "",
            page("root", deep_link),
        ),
        "/host-a/next" => (
            "200 OK",
            "text/html; charset=utf-8",
            "",
            page("next", deep_link),
        ),
        "/host-a/download" => (
            "200 OK",
            "application/octet-stream",
            "Content-Disposition: attachment; filename=pilot.txt\r\n",
            DOWNLOAD_BODY.to_vec(),
        ),
        _ => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            "",
            b"not found".to_vec(),
        ),
    };
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(headers.as_bytes());
    let _ = stream.write_all(&body);
}

fn page(page: &str, deep_link: &Value) -> Vec<u8> {
    let shell = render_shell_html();
    let script = if page == "root" {
        r#"
          window.ipc.postMessage(JSON.stringify({
            kind: "loaded",
            page: "root",
            uiRendered: document.body.innerText.includes("Add a project")
          }));
          setTimeout(() => location.assign("/host-b/private"), 25);
          setTimeout(() => location.assign("/host-a/next"), 500);
        "#
        .to_owned()
    } else {
        format!(
            r#"
              window.ipc.postMessage(JSON.stringify({{
                kind: "loaded",
                page: "next",
                uiRendered: document.body.innerText.includes("Add a project")
              }}));
              const target = {deep_link};
              window.ipc.postMessage(JSON.stringify({{
                kind: "deep_link",
                serverId: target.server_id,
                agentId: target.agent_id
              }}));
              setTimeout(() => {{
                const link = document.createElement("a");
                link.href = "/host-a/download";
                link.download = "pilot.txt";
                document.body.appendChild(link);
                link.click();
              }}, 50);
            "#
        )
    };
    format!("<!doctype html><html><body>{shell}<script>{script}</script></body></html>")
        .into_bytes()
}
