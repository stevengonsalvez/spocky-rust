//! Deterministic desktop-browser contract pilot.
//!
//! This library models browser-host decisions and traces. The companion
//! `spocky-browser-host` binary exercises those boundaries through a real
//! platform webview, without making a full-parity claim.

use std::collections::{BTreeMap, BTreeSet};

use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use spocky_contracts::text::js_trim;
use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PilotConfig {
    trusted_automation: bool,
}

impl PilotConfig {
    #[must_use]
    pub const fn all_supported() -> Self {
        Self {
            trusted_automation: true,
        }
    }

    #[must_use]
    pub const fn without_trusted_automation() -> Self {
        Self {
            trusted_automation: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TabStatus {
    Active,
    Crashed,
    Recovering,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Tab {
    workspace_id: String,
    host_id: String,
    url: String,
    webview_id: Option<String>,
    status: TabStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadStatus {
    InProgress,
    Failed { reason: String },
    Interrupted { reason: String },
    Completed { bytes: u64 },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Download {
    tab_id: String,
    url: String,
    file_name: String,
    status: DownloadStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebviewSecurity {
    LockedDown,
    Relaxed,
}

impl WebviewSecurity {
    #[must_use]
    pub const fn locked_down() -> Self {
        Self::LockedDown
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEvent {
    pub sequence: u64,
    pub operation: String,
    pub subject: String,
    pub outcome: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PilotError {
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutomationCommand<'a> {
    Click { reference: &'a str },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomationReceipt {
    pub request_id: String,
    pub webview_id: String,
    pub native_event: &'static str,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeepLinkTarget {
    pub server_id: String,
    pub agent_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeepLinkDelivery {
    Queued,
    Delivered(DeepLinkTarget),
}

#[derive(Debug, Deserialize, Serialize)]
struct RecoverySnapshot {
    tabs: BTreeMap<String, Tab>,
    downloads: BTreeMap<String, Download>,
    pending_deep_links: BTreeMap<String, DeepLinkTarget>,
    trace: Vec<TraceEvent>,
}

#[derive(Debug)]
pub struct BrowserPilot {
    config: PilotConfig,
    tabs: BTreeMap<String, Tab>,
    downloads: BTreeMap<String, Download>,
    ready_hosts: BTreeSet<String>,
    pending_deep_links: BTreeMap<String, DeepLinkTarget>,
    trace: Vec<TraceEvent>,
}

impl BrowserPilot {
    #[must_use]
    pub fn new(config: PilotConfig) -> Self {
        Self {
            config,
            tabs: BTreeMap::new(),
            downloads: BTreeMap::new(),
            ready_hosts: BTreeSet::new(),
            pending_deep_links: BTreeMap::new(),
            trace: Vec::new(),
        }
    }

    /// Opens one tab with stable caller-provided identity.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate tab identities or disallowed URLs.
    pub fn open_tab(
        &mut self,
        tab_id: &str,
        workspace_id: &str,
        host_id: &str,
        url: &str,
    ) -> Result<(), PilotError> {
        if self.tabs.contains_key(tab_id) {
            return Err(PilotError {
                code: "tab_exists",
                message: format!("Tab {tab_id} already exists"),
            });
        }
        if !is_allowed_webview_url(url) {
            return Err(PilotError {
                code: "navigation_denied",
                message: format!("URL is not allowed: {url}"),
            });
        }
        self.tabs.insert(
            tab_id.to_owned(),
            Tab {
                workspace_id: workspace_id.to_owned(),
                host_id: host_id.to_owned(),
                url: url.to_owned(),
                webview_id: None,
                status: TabStatus::Active,
            },
        );
        self.record("tab.open", tab_id, "applied", url);
        Ok(())
    }

    /// Dispatches an automation command through the trusted host boundary.
    ///
    /// # Errors
    ///
    /// Returns an explicit unsupported or isolation result when native input,
    /// host ownership, or an attached guest is absent.
    pub fn execute_automation(
        &mut self,
        request_id: &str,
        host_id: &str,
        tab_id: &str,
        command: AutomationCommand<'_>,
    ) -> Result<AutomationReceipt, PilotError> {
        if !self.config.trusted_automation {
            return self.unsupported(
                "automation.click",
                request_id,
                "trusted_input_unavailable",
                "Trusted browser input is unavailable".to_owned(),
            );
        }
        let Some(tab) = self.tabs.get(tab_id) else {
            return self.fail(
                "automation.click",
                request_id,
                "tab_not_found",
                format!("Tab {tab_id} does not exist"),
            );
        };
        if tab.host_id != host_id {
            return self.fail(
                "automation.click",
                request_id,
                "webview_isolation_denied",
                format!("Host {host_id} does not own tab {tab_id}"),
            );
        }
        let Some(webview_id) = tab.webview_id.clone() else {
            return self.fail(
                "automation.click",
                request_id,
                "webview_unavailable",
                format!("Tab {tab_id} has no attached guest"),
            );
        };
        let AutomationCommand::Click { reference } = command;
        self.record(
            "automation.click",
            request_id,
            "applied",
            &format!("{webview_id}:{reference}"),
        );
        Ok(AutomationReceipt {
            request_id: request_id.to_owned(),
            webview_id,
            native_event: "Input.dispatchMouseEvent",
        })
    }

    /// Starts a host-authorized browser download.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate identities, disallowed URLs, or host
    /// isolation violations.
    pub fn start_download(
        &mut self,
        download_id: &str,
        host_id: &str,
        tab_id: &str,
        url: &str,
        file_name: &str,
    ) -> Result<(), PilotError> {
        if self.downloads.contains_key(download_id) {
            return self.fail(
                "download.start",
                download_id,
                "download_exists",
                format!("Download {download_id} already exists"),
            );
        }
        let Some(tab) = self.tabs.get(tab_id) else {
            return self.fail(
                "download.start",
                download_id,
                "tab_not_found",
                format!("Tab {tab_id} does not exist"),
            );
        };
        if tab.host_id != host_id {
            return self.fail(
                "download.start",
                download_id,
                "webview_isolation_denied",
                format!("Host {host_id} does not own tab {tab_id}"),
            );
        }
        if !is_allowed_webview_url(url) || url == "about:blank" {
            return self.fail(
                "download.start",
                download_id,
                "download_url_denied",
                format!("Download URL is not allowed: {url}"),
            );
        }
        self.downloads.insert(
            download_id.to_owned(),
            Download {
                tab_id: tab_id.to_owned(),
                url: url.to_owned(),
                file_name: file_name.to_owned(),
                status: DownloadStatus::InProgress,
            },
        );
        self.record("download.start", download_id, "applied", file_name);
        Ok(())
    }

    /// Records a failed download.
    ///
    /// # Errors
    ///
    /// Returns an error unless the download is in progress.
    pub fn fail_download(&mut self, download_id: &str, reason: &str) -> Result<(), PilotError> {
        self.transition_download(
            download_id,
            "download.fail",
            DownloadStatus::Failed {
                reason: reason.to_owned(),
            },
            reason,
        )
    }

    /// Retries a failed download.
    ///
    /// # Errors
    ///
    /// Returns an error unless the download previously failed.
    pub fn retry_download(&mut self, download_id: &str) -> Result<(), PilotError> {
        let Some(download) = self.downloads.get(download_id) else {
            return self.download_not_found("download.retry", download_id);
        };
        if !matches!(
            download.status,
            DownloadStatus::Failed { .. } | DownloadStatus::Interrupted { .. }
        ) {
            return self.fail(
                "download.retry",
                download_id,
                "download_state_denied",
                "Only failed or interrupted downloads can retry".to_owned(),
            );
        }
        let Some(download) = self.downloads.get_mut(download_id) else {
            return self.download_not_found("download.retry", download_id);
        };
        download.status = DownloadStatus::InProgress;
        self.record("download.retry", download_id, "applied", "in_progress");
        Ok(())
    }

    /// Completes an in-progress download.
    ///
    /// # Errors
    ///
    /// Returns an error unless the download is in progress.
    pub fn complete_download(&mut self, download_id: &str, bytes: u64) -> Result<(), PilotError> {
        self.transition_download(
            download_id,
            "download.complete",
            DownloadStatus::Completed { bytes },
            &bytes.to_string(),
        )
    }

    #[must_use]
    pub fn download_status(&self, download_id: &str) -> Option<DownloadStatus> {
        Some(self.downloads.get(download_id)?.status.clone())
    }

    #[must_use]
    pub fn tab_status(&self, tab_id: &str) -> Option<TabStatus> {
        Some(self.tabs.get(tab_id)?.status)
    }

    /// Captures deterministic state after a simulated host crash.
    ///
    /// # Errors
    ///
    /// Returns an error if recovery state cannot be serialized.
    pub fn crash_snapshot(&mut self) -> Result<Vec<u8>, PilotError> {
        for tab in self.tabs.values_mut() {
            tab.status = TabStatus::Crashed;
            tab.webview_id = None;
        }
        for download in self.downloads.values_mut() {
            if download.status == DownloadStatus::InProgress {
                download.status = DownloadStatus::Interrupted {
                    reason: "host_crash".to_owned(),
                };
            }
        }
        self.ready_hosts.clear();
        self.record("host.crash", "pilot", "failed", "state_captured");
        serde_json::to_vec(&RecoverySnapshot {
            tabs: self.tabs.clone(),
            downloads: self.downloads.clone(),
            pending_deep_links: self.pending_deep_links.clone(),
            trace: self.trace.clone(),
        })
        .map_err(|error| PilotError {
            code: "snapshot_serialize_failed",
            message: error.to_string(),
        })
    }

    /// Restores deterministic pilot state after a simulated host restart.
    ///
    /// # Errors
    ///
    /// Returns an error when recovery bytes are invalid.
    pub fn restart(config: PilotConfig, snapshot: &[u8]) -> Result<Self, PilotError> {
        let RecoverySnapshot {
            mut tabs,
            downloads,
            pending_deep_links,
            trace,
        } = serde_json::from_slice(snapshot).map_err(|error| PilotError {
            code: "snapshot_invalid",
            message: error.to_string(),
        })?;
        for tab in tabs.values_mut() {
            tab.status = TabStatus::Recovering;
            tab.webview_id = None;
        }
        let mut pilot = Self {
            config,
            tabs,
            downloads,
            ready_hosts: BTreeSet::new(),
            pending_deep_links,
            trace,
        };
        pilot.record("host.restart", "pilot", "applied", "state_restored");
        Ok(pilot)
    }

    /// Parses and routes an exact Paseo agent deep link.
    ///
    /// # Errors
    ///
    /// Returns a denial for any scheme, authority, query, fragment, or path
    /// outside `paseo://h/{server}/agent/{agent}`.
    pub fn receive_deep_link(
        &mut self,
        host_id: &str,
        input: &str,
    ) -> Result<DeepLinkDelivery, PilotError> {
        let Some(target) = parse_deep_link(input) else {
            return self.fail(
                "deep_link.receive",
                host_id,
                "deep_link_invalid",
                format!("Invalid agent deep link: {input}"),
            );
        };
        if self.ready_hosts.contains(host_id) {
            self.record(
                "deep_link.deliver",
                host_id,
                "applied",
                &format!("{}:{}", target.server_id, target.agent_id),
            );
            return Ok(DeepLinkDelivery::Delivered(target));
        }
        self.pending_deep_links
            .insert(host_id.to_owned(), target.clone());
        self.record("deep_link.queue", host_id, "applied", &target.agent_id);
        Ok(DeepLinkDelivery::Queued)
    }

    #[must_use]
    pub fn host_ready(&mut self, host_id: &str) -> Option<DeepLinkTarget> {
        self.ready_hosts.insert(host_id.to_owned());
        let target = self.pending_deep_links.remove(host_id)?;
        self.record(
            "deep_link.deliver",
            host_id,
            "applied",
            &format!("{}:{}", target.server_id, target.agent_id),
        );
        Some(target)
    }

    /// Attaches a sandboxed guest to its owning host.
    ///
    /// # Errors
    ///
    /// Returns an explicit denial for a missing tab, host mismatch, or weaker
    /// guest security policy.
    pub fn attach_webview(
        &mut self,
        tab_id: &str,
        host_id: &str,
        webview_id: &str,
        security: WebviewSecurity,
    ) -> Result<(), PilotError> {
        let Some(tab) = self.tabs.get(tab_id) else {
            return self.fail(
                "webview.attach",
                tab_id,
                "tab_not_found",
                format!("Tab {tab_id} does not exist"),
            );
        };
        if tab.host_id != host_id {
            return self.fail(
                "webview.attach",
                tab_id,
                "webview_isolation_denied",
                format!("Host {host_id} does not own tab {tab_id}"),
            );
        }
        if security != WebviewSecurity::locked_down() {
            return self.fail(
                "webview.attach",
                tab_id,
                "webview_security_denied",
                "Guest security policy is not locked down".to_owned(),
            );
        }
        let Some(tab) = self.tabs.get_mut(tab_id) else {
            return self.fail(
                "webview.attach",
                tab_id,
                "tab_not_found",
                format!("Tab {tab_id} does not exist"),
            );
        };
        tab.webview_id = Some(webview_id.to_owned());
        tab.status = TabStatus::Active;
        self.record("webview.attach", tab_id, "applied", webview_id);
        Ok(())
    }

    #[must_use]
    pub fn webview_id(&self, tab_id: &str) -> Option<&str> {
        self.tabs.get(tab_id)?.webview_id.as_deref()
    }

    #[must_use]
    pub fn tab_ids(&self) -> Vec<&str> {
        self.tabs.keys().map(String::as_str).collect()
    }

    /// Serializes the ordered trace as newline-delimited JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if a trace event cannot be serialized.
    pub fn trace_json_lines(&self) -> Result<String, serde_json::Error> {
        self.trace
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .map(|lines| lines.join("\n"))
    }

    fn record(&mut self, operation: &str, subject: &str, outcome: &str, detail: &str) {
        self.trace.push(TraceEvent {
            sequence: self.trace.len() as u64 + 1,
            operation: operation.to_owned(),
            subject: subject.to_owned(),
            outcome: outcome.to_owned(),
            detail: detail.to_owned(),
        });
    }

    fn fail<T>(
        &mut self,
        operation: &str,
        subject: &str,
        code: &'static str,
        message: String,
    ) -> Result<T, PilotError> {
        self.record(operation, subject, "denied", code);
        Err(PilotError { code, message })
    }

    fn unsupported<T>(
        &mut self,
        operation: &str,
        subject: &str,
        code: &'static str,
        message: String,
    ) -> Result<T, PilotError> {
        self.record(operation, subject, "unsupported", code);
        Err(PilotError { code, message })
    }

    fn transition_download(
        &mut self,
        download_id: &str,
        operation: &str,
        status: DownloadStatus,
        detail: &str,
    ) -> Result<(), PilotError> {
        let Some(download) = self.downloads.get(download_id) else {
            return self.download_not_found(operation, download_id);
        };
        if download.status != DownloadStatus::InProgress {
            return self.fail(
                operation,
                download_id,
                "download_state_denied",
                "Download is not in progress".to_owned(),
            );
        }
        self.downloads
            .get_mut(download_id)
            .expect("download existence checked")
            .status = status;
        self.record(operation, download_id, "applied", detail);
        Ok(())
    }

    fn download_not_found<T>(
        &mut self,
        operation: &str,
        download_id: &str,
    ) -> Result<T, PilotError> {
        self.fail(
            operation,
            download_id,
            "download_not_found",
            format!("Download {download_id} does not exist"),
        )
    }
}

fn is_allowed_webview_url(value: &str) -> bool {
    if value == "about:blank" {
        return true;
    }
    Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

fn parse_deep_link(input: &str) -> Option<DeepLinkTarget> {
    let url = Url::parse(input).ok()?;
    if url.scheme() != "paseo"
        || url.host_str() != Some("h")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let segments = url
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() != 3 || segments[1] != "agent" {
        return None;
    }
    let server_id = js_trim(&percent_decode_str(segments[0]).decode_utf8().ok()?).to_owned();
    let agent_id = js_trim(&percent_decode_str(segments[2]).decode_utf8().ok()?).to_owned();
    if server_id.is_empty() || agent_id.is_empty() {
        return None;
    }
    Some(DeepLinkTarget {
        server_id,
        agent_id,
    })
}
