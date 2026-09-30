use spocky_browser_pilot::{
    AutomationCommand, BrowserPilot, DeepLinkDelivery, DeepLinkTarget, DownloadStatus, PilotConfig,
    TabStatus, WebviewSecurity,
};
use std::process::Command;

#[test]
fn tabs_have_stable_identity_and_ordered_protocol_trace() {
    let mut pilot = BrowserPilot::new(PilotConfig::all_supported());

    pilot
        .open_tab("tab-b", "workspace-1", "host-1", "https://example.test/b")
        .expect("tab opens");
    pilot
        .open_tab("tab-a", "workspace-1", "host-1", "about:blank")
        .expect("tab opens");

    assert_eq!(pilot.tab_ids(), ["tab-a", "tab-b"]);
    assert_eq!(
        pilot.trace_json_lines().expect("trace serializes"),
        concat!(
            "{\"sequence\":1,\"operation\":\"tab.open\",\"subject\":\"tab-b\",\"outcome\":\"applied\",\"detail\":\"https://example.test/b\"}\n",
            "{\"sequence\":2,\"operation\":\"tab.open\",\"subject\":\"tab-a\",\"outcome\":\"applied\",\"detail\":\"about:blank\"}"
        )
    );
}

#[test]
fn automation_dispatches_trusted_input_only_to_the_attached_guest() {
    let mut pilot = BrowserPilot::new(PilotConfig::all_supported());
    pilot
        .open_tab("tab-1", "workspace-1", "host-1", "https://example.test")
        .unwrap();
    pilot
        .attach_webview("tab-1", "host-1", "guest-7", WebviewSecurity::locked_down())
        .unwrap();

    let receipt = pilot
        .execute_automation(
            "request-9",
            "host-1",
            "tab-1",
            AutomationCommand::Click { reference: "e4" },
        )
        .expect("trusted click is dispatched");

    assert_eq!(receipt.request_id, "request-9");
    assert_eq!(receipt.webview_id, "guest-7");
    assert_eq!(receipt.native_event, "Input.dispatchMouseEvent");
    assert!(pilot.trace_json_lines().unwrap().contains(
        "\"operation\":\"automation.click\",\"subject\":\"request-9\",\"outcome\":\"applied\",\"detail\":\"guest-7:e4\""
    ));
}

#[test]
fn unavailable_trusted_input_is_explicitly_unsupported() {
    let mut pilot = BrowserPilot::new(PilotConfig::without_trusted_automation());
    pilot
        .open_tab("tab-1", "workspace-1", "host-1", "https://example.test")
        .unwrap();

    let error = pilot
        .execute_automation(
            "request-10",
            "host-1",
            "tab-1",
            AutomationCommand::Click { reference: "e4" },
        )
        .expect_err("pilot must not emulate untrusted input");

    assert_eq!(error.code, "trusted_input_unavailable");
    assert!(pilot.trace_json_lines().unwrap().contains(
        "\"operation\":\"automation.click\",\"subject\":\"request-10\",\"outcome\":\"unsupported\",\"detail\":\"trusted_input_unavailable\""
    ));
}

#[test]
fn downloads_expose_failure_retry_and_completion_transitions() {
    let mut pilot = BrowserPilot::new(PilotConfig::all_supported());
    pilot
        .open_tab("tab-1", "workspace-1", "host-1", "https://example.test")
        .unwrap();

    pilot
        .start_download(
            "download-1",
            "host-1",
            "tab-1",
            "https://example.test/archive.zip",
            "archive.zip",
        )
        .expect("download starts");
    pilot
        .fail_download("download-1", "network_reset")
        .expect("failure records");
    pilot.retry_download("download-1").expect("retry starts");
    pilot
        .complete_download("download-1", 4096)
        .expect("download completes");

    assert_eq!(
        pilot.download_status("download-1"),
        Some(DownloadStatus::Completed { bytes: 4096 })
    );
    let trace = pilot.trace_json_lines().unwrap();
    assert!(trace.contains("\"operation\":\"download.fail\""));
    assert!(trace.contains("\"operation\":\"download.retry\""));
    assert!(trace.contains("\"operation\":\"download.complete\""));
}

#[test]
fn deep_links_queue_during_loading_and_deliver_once_ready() {
    let mut pilot = BrowserPilot::new(PilotConfig::all_supported());

    let delivery = pilot
        .receive_deep_link("host-1", "paseo://h/server%2Fmain/agent/agent%20123")
        .expect("exact deep link parses");

    assert_eq!(delivery, DeepLinkDelivery::Queued);
    assert_eq!(
        pilot.host_ready("host-1"),
        Some(DeepLinkTarget {
            server_id: "server/main".to_owned(),
            agent_id: "agent 123".to_owned(),
        })
    );
    assert_eq!(pilot.host_ready("host-1"), None);
    assert!(pilot.trace_json_lines().unwrap().contains(
        "\"operation\":\"deep_link.deliver\",\"subject\":\"host-1\",\"outcome\":\"applied\""
    ));
}

#[test]
fn crash_snapshot_recovers_tabs_downloads_and_queued_deep_links() {
    let config = PilotConfig::all_supported();
    let mut pilot = BrowserPilot::new(config);
    pilot
        .open_tab("tab-1", "workspace-1", "host-1", "https://example.test")
        .unwrap();
    pilot
        .attach_webview("tab-1", "host-1", "guest-7", WebviewSecurity::locked_down())
        .unwrap();
    pilot
        .start_download(
            "download-1",
            "host-1",
            "tab-1",
            "https://example.test/archive.zip",
            "archive.zip",
        )
        .unwrap();
    pilot
        .receive_deep_link("host-1", "paseo://h/server-1/agent/agent-1")
        .unwrap();

    let snapshot = pilot.crash_snapshot().expect("snapshot serializes");
    let mut restarted = BrowserPilot::restart(config, &snapshot).expect("snapshot restores");

    assert_eq!(restarted.tab_status("tab-1"), Some(TabStatus::Recovering));
    assert_eq!(restarted.webview_id("tab-1"), None);
    assert_eq!(
        restarted.download_status("download-1"),
        Some(DownloadStatus::Interrupted {
            reason: "host_crash".to_owned(),
        })
    );
    restarted
        .attach_webview("tab-1", "host-1", "guest-8", WebviewSecurity::locked_down())
        .expect("guest reattaches");
    restarted
        .retry_download("download-1")
        .expect("interrupted download retries");
    assert_eq!(restarted.tab_status("tab-1"), Some(TabStatus::Active));
    assert_eq!(
        restarted.host_ready("host-1"),
        Some(DeepLinkTarget {
            server_id: "server-1".to_owned(),
            agent_id: "agent-1".to_owned(),
        })
    );
    assert!(
        restarted.trace_json_lines().unwrap().contains(
            "\"operation\":\"host.restart\",\"subject\":\"pilot\",\"outcome\":\"applied\""
        )
    );
}

#[test]
fn executable_emits_repeatable_protocol_and_limitation_evidence() {
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_spocky-browser-pilot"))
            .output()
            .expect("pilot executable runs")
    };

    let first = run();
    let second = run();

    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    let stdout = String::from_utf8(first.stdout).expect("stdout is UTF-8");
    assert!(stdout.contains("\"operation\":\"automation.click\""));
    assert!(stdout.contains("\"operation\":\"host.restart\""));
    assert!(stdout.contains("\"outcome\":\"unsupported\""));
    assert!(stdout.contains("limitation=contract-model-only"));
}

#[test]
fn webviews_require_locked_security_and_the_owning_host() {
    let mut pilot = BrowserPilot::new(PilotConfig::all_supported());
    pilot
        .open_tab("tab-1", "workspace-1", "host-1", "https://example.test")
        .expect("tab opens");

    pilot
        .attach_webview("tab-1", "host-1", "guest-7", WebviewSecurity::locked_down())
        .expect("secure guest attaches");
    let error = pilot
        .attach_webview("tab-1", "host-2", "guest-8", WebviewSecurity::locked_down())
        .expect_err("another host cannot claim the guest");

    assert_eq!(error.code, "webview_isolation_denied");
    assert_eq!(pilot.webview_id("tab-1"), Some("guest-7"));
    assert!(
        pilot.trace_json_lines().unwrap().contains(
            "\"operation\":\"webview.attach\",\"subject\":\"tab-1\",\"outcome\":\"denied\""
        )
    );
}
