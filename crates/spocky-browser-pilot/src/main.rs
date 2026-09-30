use spocky_browser_pilot::{AutomationCommand, BrowserPilot, PilotConfig, WebviewSecurity};

fn main() {
    let config = PilotConfig::all_supported();
    let mut pilot = BrowserPilot::new(config);
    pilot
        .open_tab("tab-1", "workspace-1", "host-1", "https://example.test")
        .expect("scenario tab opens");
    pilot
        .attach_webview("tab-1", "host-1", "guest-1", WebviewSecurity::locked_down())
        .expect("scenario guest attaches");
    pilot
        .execute_automation(
            "request-1",
            "host-1",
            "tab-1",
            AutomationCommand::Click { reference: "e1" },
        )
        .expect("scenario trusted input dispatches");
    pilot
        .start_download(
            "download-1",
            "host-1",
            "tab-1",
            "https://example.test/archive.zip",
            "archive.zip",
        )
        .expect("scenario download starts");
    pilot
        .receive_deep_link("host-1", "paseo://h/server-1/agent/agent-1")
        .expect("scenario deep link queues");
    let snapshot = pilot
        .crash_snapshot()
        .expect("scenario snapshot serializes");
    let mut restarted =
        BrowserPilot::restart(config, &snapshot).expect("scenario snapshot restores");
    restarted
        .attach_webview("tab-1", "host-1", "guest-2", WebviewSecurity::locked_down())
        .expect("scenario guest recovers");
    restarted
        .retry_download("download-1")
        .expect("scenario download retries");
    restarted
        .complete_download("download-1", 4096)
        .expect("scenario download completes");
    let _delivered = restarted.host_ready("host-1");

    let mut unsupported = BrowserPilot::new(PilotConfig::without_trusted_automation());
    unsupported
        .open_tab("tab-u", "workspace-1", "host-1", "about:blank")
        .expect("unsupported scenario tab opens");
    let _unsupported = unsupported.execute_automation(
        "request-u",
        "host-1",
        "tab-u",
        AutomationCommand::Click { reference: "e1" },
    );

    println!("scenario=supported");
    println!(
        "{}",
        restarted.trace_json_lines().expect("trace serializes")
    );
    println!("scenario=unsupported");
    println!(
        "{}",
        unsupported.trace_json_lines().expect("trace serializes")
    );
    println!("limitation=contract-model-only");
    println!("limitation=no-embedded-webview-or-network-transfer");
    println!("limitation=no-visual-capture-or-platform-launch-evidence");
}
