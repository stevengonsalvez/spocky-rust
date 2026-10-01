//! Offline Hub trigger, lease, execution and GitHub webhook contract pilot.
//!
//! Each behavior mirrors the pinned Hub baseline; see `evidence/phase2/hub-triggers-enumeration.md`
//! for the covered surface and the remaining gaps.

mod identity;
mod manual;
pub mod timezone;
mod webhook;

use std::collections::BTreeMap;

use timezone::HostTimeZone;

pub use identity::durable_execution_id;
pub use manual::{
    AuthOutcome, DispatchedRun, ManualDispatchError, ManualEvent, ManualHttpResponse,
    ManualParseFailure, ManualRunMatch, ManualRunPayload, ManualRunRejection, ManualRunResult,
    ManualTriggerInput, PublicResponse, RunConfiguration, RunTrigger, match_manual_run,
    parse_manual_payload, public_manual_run,
};
pub use webhook::{
    AcceptCall, AcceptFailure, Acceptance, GitHubWebhook, GitHubWebhookRequest, LifecycleCall,
    MAX_WEBHOOK_BYTES, WebhookBackend, WebhookHttpResponse, github_signature, hash_signature,
    verify_github_signature,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionStatus {
    Spawning,
    Running,
    Succeeded,
    Failed,
}

impl ExecutionStatus {
    const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Spawning => "spawning",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRecord {
    pub id: String,
    pub run_id: String,
    pub step_run_id: String,
    pub status: ExecutionStatus,
    /// The requested deadline capped by the run deadline.
    pub deadline_at_ms: u64,
    /// The requested idle deadline capped by `deadline_at_ms`; cleared on a terminal status.
    pub idle_deadline_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionTransition {
    pub execution: ExecutionRecord,
    pub transitioned: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionReservation {
    Created(ExecutionRecord),
    /// The step already has an execution; it is returned unchanged.
    Existing(ExecutionRecord),
    /// The run is not `running`; no execution exists or is created.
    RunNotRunning,
    /// The run deadline has passed. The baseline also times the run out; that side effect is not
    /// modelled here.
    DeadlineElapsed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WakeupLease {
    pub run_id: String,
    pub lease_expires_at_ms: u64,
    pub leased_before_claim: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Receipt {
    pub id: String,
    pub organization_id: String,
    pub provider: &'static str,
    pub source: String,
    pub delivery_id: String,
    pub dropped_reason: Option<&'static str>,
    pub connection_id: Option<String>,
    pub resource_id: Option<String>,
    pub received_at_ms: i64,
    project_id: String,
    configuration_revision_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedRunInput {
    pub receipt_id: String,
    pub project_id: String,
    pub configuration_revision_id: String,
    pub configured_trigger_name: String,
    pub step_ids: Vec<String>,
    pub deadline_at_ms: u64,
    pub created_at_ms: u64,
    /// Caller-chosen run ID; the baseline also accepts one and otherwise generates it.
    pub run_id: Option<String>,
    /// Caller-chosen step run IDs, one per step; generated when absent.
    pub step_run_ids: Option<Vec<String>>,
}

/// One execution reservation request for a workflow step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionRequest {
    pub step_id: String,
    pub ordinal: usize,
    pub started_at_ms: u64,
    pub deadline_at_ms: u64,
    pub idle_deadline_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunCreation {
    pub run_id: String,
    pub created: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriggerRun {
    pub id: String,
    pub status: &'static str,
    configuration_revision_id: String,
    configured_trigger_name: String,
    deadline_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunTransition {
    pub status: &'static str,
    pub transitioned: bool,
}

#[derive(Clone, Debug)]
struct StepRun {
    id: String,
    step_id: String,
    ordinal: usize,
    execution_id: Option<String>,
}

#[derive(Clone, Debug)]
struct Wakeup {
    seq: u64,
    available_at_ms: u64,
    lease_expires_at_ms: Option<u64>,
}

pub struct TriggerStore {
    next_id: u64,
    next_wakeup_seq: u64,
    time_zone: HostTimeZone,
    projects: BTreeMap<String, (String, String)>,
    receipts: BTreeMap<String, Receipt>,
    receipt_by_delivery: BTreeMap<(String, String), String>,
    runs: BTreeMap<String, TriggerRun>,
    run_by_branch: BTreeMap<(String, String, String), String>,
    steps: BTreeMap<String, Vec<StepRun>>,
    wakeups: BTreeMap<String, Wakeup>,
    executions: BTreeMap<String, ExecutionRecord>,
}

impl Default for TriggerStore {
    /// An empty store in the host time zone, read once here as the baseline process reads it.
    fn default() -> Self {
        Self::with_time_zone(HostTimeZone::from_env())
    }
}

impl TriggerStore {
    /// An empty store whose `receivedAt` date-times without an offset use `time_zone`.
    #[must_use]
    pub fn with_time_zone(time_zone: HostTimeZone) -> Self {
        Self {
            next_id: 0,
            next_wakeup_seq: 0,
            time_zone,
            projects: BTreeMap::new(),
            receipts: BTreeMap::new(),
            receipt_by_delivery: BTreeMap::new(),
            runs: BTreeMap::new(),
            run_by_branch: BTreeMap::new(),
            steps: BTreeMap::new(),
            wakeups: BTreeMap::new(),
            executions: BTreeMap::new(),
        }
    }

    /// Registers a project with its active configuration revision.
    pub fn register_project(
        &mut self,
        organization_id: &str,
        project_id: &str,
        active_revision_id: &str,
    ) {
        self.projects.insert(
            project_id.to_owned(),
            (organization_id.to_owned(), active_revision_id.to_owned()),
        );
    }

    /// Pins the host offset applied to `receivedAt` date-times written without an offset.
    pub fn set_local_offset_minutes(&mut self, minutes: i32) {
        self.time_zone = HostTimeZone::fixed_minutes(minutes);
    }

    /// Minutes east of UTC that the store applies to the local wall-clock time `local_ms`, the
    /// wall clock read as if it were UTC. The baseline reports it as `-getTimezoneOffset()`; both
    /// drop the seconds of a historic local mean time offset (Sao Paulo before 1914 is -3:06:28 and
    /// reads as -186 minutes), while `receivedAt` keeps them.
    #[must_use]
    pub fn local_offset_minutes_at(&self, local_ms: i64) -> i64 {
        use timezone::LocalOffset;
        // An offset is at most a few hours, so the conversion never saturates.
        i64::try_from(self.time_zone.offset_ms_at_local(i128::from(local_ms)) / 60_000)
            .unwrap_or_default()
    }

    /// Handles one manual trigger request end to end: parse, persist, dispatch.
    ///
    /// `handler` is the started trigger handler, if any; without one the receipt is marked
    /// dropped with `configuration_unavailable`. `now_ms` stands in for `new Date()` when the
    /// request omits `receivedAt`.
    ///
    /// # Errors
    ///
    /// Returns [`ManualDispatchError`] when the project has no active configuration; the baseline
    /// lets that error escape the request handler.
    pub fn handle_manual_request(
        &mut self,
        handler: Option<&mut dyn FnMut(ManualEvent)>,
        now_ms: i64,
        body: &[u8],
    ) -> Result<ManualHttpResponse, ManualDispatchError> {
        let input = match parse_manual_payload(body, &self.time_zone) {
            Ok(input) => input,
            Err(ManualParseFailure::InvalidJson) => {
                return Ok(ManualHttpResponse {
                    status: 400,
                    body: manual::manual_json_error("request body must be valid JSON"),
                });
            }
            Err(ManualParseFailure::Invalid(message)) => {
                return Ok(ManualHttpResponse {
                    status: 400,
                    body: manual::manual_json_error(&message),
                });
            }
        };
        let receipt = self.persist_manual(&input, now_ms)?;
        // A replayed delivery re-enters the handler: run creation, not intake, is idempotent.
        if let Some(handler) = handler {
            handler(ManualEvent {
                receipt_id: receipt.id.clone(),
                organization_id: receipt.organization_id.clone(),
                project_id: receipt.project_id.clone(),
                configuration_revision_id: receipt.configuration_revision_id.clone(),
                source: receipt.source.clone(),
                delivery_id: input.delivery_id.clone(),
                connection_id: receipt.connection_id.clone(),
                resource_id: receipt.resource_id.clone(),
                received_at_ms: receipt.received_at_ms,
            });
        } else if let Some(stored) = self.receipts.get_mut(&receipt.id) {
            stored
                .dropped_reason
                .get_or_insert("configuration_unavailable");
        }
        Ok(ManualHttpResponse {
            status: 200,
            body: manual::manual_accepted_body(&input.delivery_id),
        })
    }

    fn persist_manual(
        &mut self,
        input: &ManualTriggerInput,
        now_ms: i64,
    ) -> Result<Receipt, ManualDispatchError> {
        let key = (input.organization_id.clone(), input.delivery_id.clone());
        if let Some(existing) = self
            .receipt_by_delivery
            .get(&key)
            .and_then(|id| self.receipts.get(id))
        {
            return Ok(existing.clone());
        }
        let revision_id = match self.projects.get(&input.project_id) {
            Some((organization_id, revision_id)) if *organization_id == input.organization_id => {
                revision_id.clone()
            }
            _ => return Err(ManualDispatchError::ProjectConfigurationUnavailable),
        };
        let receipt = Receipt {
            id: self.allocate_id("receipt"),
            organization_id: input.organization_id.clone(),
            provider: "manual",
            source: input.source.clone(),
            delivery_id: input.delivery_id.clone(),
            dropped_reason: None,
            connection_id: input.connection_id.clone(),
            resource_id: input.resource_id.clone(),
            received_at_ms: input.received_at_ms.unwrap_or(now_ms),
            project_id: input.project_id.clone(),
            configuration_revision_id: revision_id,
        };
        self.receipt_by_delivery.insert(key, receipt.id.clone());
        self.receipts.insert(receipt.id.clone(), receipt.clone());
        Ok(receipt)
    }

    #[must_use]
    pub fn receipt(&self, organization_id: &str, delivery_id: &str) -> Option<&Receipt> {
        self.receipt_by_delivery
            .get(&(organization_id.to_owned(), delivery_id.to_owned()))
            .and_then(|id| self.receipts.get(id))
    }

    /// One run per receipt, project and configured trigger name; a repeat returns the first.
    pub fn create_accepted_run(&mut self, input: &AcceptedRunInput) -> RunCreation {
        let branch = (
            input.receipt_id.clone(),
            input.project_id.clone(),
            input.configured_trigger_name.clone(),
        );
        if let Some(run_id) = self.run_by_branch.get(&branch) {
            return RunCreation {
                run_id: run_id.clone(),
                created: false,
            };
        }
        let run_id = input
            .run_id
            .clone()
            .unwrap_or_else(|| self.allocate_id("run"));
        self.runs.insert(
            run_id.clone(),
            TriggerRun {
                id: run_id.clone(),
                status: "running",
                configuration_revision_id: input.configuration_revision_id.clone(),
                configured_trigger_name: input.configured_trigger_name.clone(),
                deadline_at_ms: input.deadline_at_ms,
            },
        );
        self.run_by_branch.insert(branch, run_id.clone());
        let mut steps = Vec::new();
        for (ordinal, step_id) in input.step_ids.iter().enumerate() {
            let id = match input.step_run_ids.as_ref().and_then(|ids| ids.get(ordinal)) {
                Some(id) => id.clone(),
                None => self.allocate_id("step-run"),
            };
            steps.push(StepRun {
                id,
                step_id: step_id.clone(),
                ordinal,
                execution_id: None,
            });
        }
        self.steps.insert(run_id.clone(), steps);
        self.next_wakeup_seq += 1;
        self.wakeups.insert(
            run_id.clone(),
            Wakeup {
                seq: self.next_wakeup_seq,
                available_at_ms: input.created_at_ms,
                lease_expires_at_ms: None,
            },
        );
        RunCreation {
            run_id,
            created: true,
        }
    }

    /// Claims the earliest available wakeup whose lease is absent or expired; ties go to the
    /// wakeup created first.
    pub fn claim_wakeup(&mut self, now_ms: u64, lease_ms: u64) -> Option<WakeupLease> {
        let run_id = self
            .wakeups
            .iter()
            .filter(|(_, wakeup)| {
                wakeup.available_at_ms <= now_ms
                    && wakeup
                        .lease_expires_at_ms
                        .is_none_or(|expiry| expiry <= now_ms)
            })
            .min_by_key(|(_, wakeup)| (wakeup.available_at_ms, wakeup.seq))
            .map(|(run_id, _)| run_id.clone())?;
        let wakeup = self.wakeups.get_mut(&run_id)?;
        let leased_before_claim = wakeup.lease_expires_at_ms.is_some();
        let lease_expires_at_ms = now_ms.saturating_add(lease_ms);
        wakeup.lease_expires_at_ms = Some(lease_expires_at_ms);
        Some(WakeupLease {
            run_id,
            lease_expires_at_ms,
            leased_before_claim,
        })
    }

    /// Releases a lease only while the claimed expiry is still the stored one.
    pub fn release_wakeup(&mut self, lease: &WakeupLease, now_ms: u64) {
        if let Some(wakeup) = self.wakeups.get_mut(&lease.run_id)
            && wakeup.lease_expires_at_ms == Some(lease.lease_expires_at_ms)
        {
            wakeup.lease_expires_at_ms = Some(now_ms);
        }
    }

    /// Reserves the execution for the step matching `stepId` and `ordinal`; the ID is derived, so
    /// recovery reuses the record.
    pub fn reserve_execution(
        &mut self,
        run_id: &str,
        request: &ExecutionRequest,
    ) -> Option<ExecutionReservation> {
        let step_index =
            self.steps.get(run_id)?.iter().position(|step| {
                step.step_id == request.step_id && step.ordinal == request.ordinal
            })?;
        let step = self.steps.get(run_id)?.get(step_index)?.clone();
        if let Some(existing) = step
            .execution_id
            .as_ref()
            .and_then(|id| self.executions.get(id))
        {
            return Some(ExecutionReservation::Existing(existing.clone()));
        }
        let run = self.runs.get(run_id)?;
        if run.status != "running" {
            return Some(ExecutionReservation::RunNotRunning);
        }
        if run.deadline_at_ms <= request.started_at_ms {
            return Some(ExecutionReservation::DeadlineElapsed);
        }
        let deadline_at_ms = request.deadline_at_ms.min(run.deadline_at_ms);
        let execution = ExecutionRecord {
            id: durable_execution_id(
                run_id,
                &run.configuration_revision_id,
                &run.configured_trigger_name,
                Some(&step.id),
            ),
            run_id: run_id.to_owned(),
            step_run_id: step.id,
            status: ExecutionStatus::Spawning,
            deadline_at_ms,
            idle_deadline_at_ms: Some(request.idle_deadline_at_ms.min(deadline_at_ms)),
            completed_at_ms: None,
        };
        self.steps
            .get_mut(run_id)?
            .get_mut(step_index)?
            .execution_id = Some(execution.id.clone());
        self.executions
            .insert(execution.id.clone(), execution.clone());
        Some(ExecutionReservation::Created(execution))
    }

    /// Moves an execution forward; the first terminal status wins and later attempts are no-ops.
    pub fn transition_execution(
        &mut self,
        execution_id: &str,
        status: ExecutionStatus,
        observed_at_ms: u64,
    ) -> Option<ExecutionTransition> {
        let execution = self.executions.get_mut(execution_id)?;
        if execution.status.is_terminal() {
            return Some(ExecutionTransition {
                execution: execution.clone(),
                transitioned: false,
            });
        }
        execution.status = status;
        if status.is_terminal() {
            execution.completed_at_ms = Some(observed_at_ms);
            execution.idle_deadline_at_ms = None;
        }
        Some(ExecutionTransition {
            execution: execution.clone(),
            transitioned: true,
        })
    }

    /// Marks a running run succeeded and drops its wakeup; a non-running run is left alone.
    pub fn succeed_run(&mut self, run_id: &str) -> Option<RunTransition> {
        let run = self.runs.get_mut(run_id)?;
        if run.status != "running" {
            return Some(RunTransition {
                status: run.status,
                transitioned: false,
            });
        }
        run.status = "succeeded";
        self.wakeups.remove(run_id);
        Some(RunTransition {
            status: "succeeded",
            transitioned: true,
        })
    }

    #[must_use]
    pub fn run(&self, run_id: &str) -> Option<&TriggerRun> {
        self.runs.get(run_id)
    }

    #[must_use]
    pub fn step_run_id(&self, run_id: &str, step_id: &str, ordinal: usize) -> Option<&str> {
        self.steps
            .get(run_id)?
            .iter()
            .find(|step| step.step_id == step_id && step.ordinal == ordinal)
            .map(|step| step.id.as_str())
    }

    #[must_use]
    pub fn receipt_count(&self) -> usize {
        self.receipts.len()
    }

    #[must_use]
    pub fn run_count(&self) -> usize {
        self.runs.len()
    }

    #[must_use]
    pub fn execution_count(&self) -> usize {
        self.executions.len()
    }

    fn allocate_id(&mut self, kind: &str) -> String {
        self.next_id += 1;
        format!("{kind}-{:08}", self.next_id)
    }
}
