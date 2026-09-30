#[cfg(target_os = "macos")]
#[path = "spocky-browser-host/macos.rs"]
mod macos;

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("spocky-browser-host pilot currently requires macOS WKWebView");
    std::process::ExitCode::from(2)
}
