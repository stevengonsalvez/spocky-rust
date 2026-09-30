use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{
    Account, AccountId, DurableHubStore, HubError, HubPilot, OrganizationId, Role, fingerprint,
};

const INVITATION_LIFETIME_SECONDS: u64 = 48 * 60 * 60;
const INVITATION_BASE_URL: &str = "https://hub.example.test";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InvitationRole {
    Admin,
    Member,
}

impl From<InvitationRole> for Role {
    fn from(role: InvitationRole) -> Self {
        match role {
            InvitationRole::Admin => Self::Admin,
            InvitationRole::Member => Self::Member,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvitationSummary {
    pub id: String,
    pub email: String,
    pub role: InvitationRole,
    pub expires_at_epoch_seconds: u64,
    pub link: String,
}

pub struct InvitationEmail<'a> {
    pub id: &'a str,
    pub email: &'a str,
    pub inviter_name: &'a str,
    pub organization_name: &'a str,
    pub role: InvitationRole,
    pub link: &'a str,
    pub expires_at_iso: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvitationEmailMessage {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
    pub idempotency_key: String,
}

#[must_use]
pub fn render_invitation_email(invitation: &InvitationEmail<'_>) -> InvitationEmailMessage {
    let role = match invitation.role {
        InvitationRole::Admin => "an admin",
        InvitationRole::Member => "a member",
    };
    let introduction = format!(
        "{} invited you to join {} as {role}.",
        invitation.inviter_name, invitation.organization_name
    );
    let expiry = format!("This invitation expires at {}.", invitation.expires_at_iso);
    InvitationEmailMessage {
        to: invitation.email.to_owned(),
        subject: format!("Join {} on Paseo", invitation.organization_name),
        text: format!(
            "{introduction}\n\nAccept the invitation: {}\n\n{expiry}",
            invitation.link
        ),
        html: format!(
            "<p>{}</p><p><a href=\"{}\">Join {}</a></p><p>{}</p>",
            escape_html(&introduction),
            escape_html(invitation.link),
            escape_html(invitation.organization_name),
            escape_html(&expiry)
        ),
        idempotency_key: format!("paseo-invitation-{}", invitation.id),
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum InvitationStatus {
    Pending,
    Canceled,
    Accepted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredInvitation {
    id: String,
    organization: OrganizationId,
    email: String,
    role: InvitationRole,
    status: InvitationStatus,
    expires_at_epoch_seconds: u64,
    created_sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct InvitationEntitlements {
    enabled: bool,
    seat_limit: Option<usize>,
}

impl<S: DurableHubStore> HubPilot<S> {
    pub fn set_invitation_entitlements(
        &mut self,
        actor: &AccountId,
        organization: &OrganizationId,
        enabled: bool,
        seat_limit: Option<usize>,
    ) -> Result<(), HubError> {
        if !self.authorize(actor, organization)?.can_manage_resources() {
            return Err(HubError::InvitationManagementRequired);
        }
        self.state.invitation_entitlements.insert(
            organization.clone(),
            InvitationEntitlements {
                enabled,
                seat_limit,
            },
        );
        self.persist()
    }

    pub fn create_invitation(
        &mut self,
        actor: &AccountId,
        organization: &OrganizationId,
        email: &str,
        role: InvitationRole,
    ) -> Result<InvitationSummary, HubError> {
        if !self.authorize(actor, organization)?.can_manage_resources() {
            return Err(HubError::InvitationManagementRequired);
        }
        let email = normalize_email(email).ok_or(HubError::InvalidInvitationInput)?;
        if self
            .state
            .memberships
            .get(organization)
            .is_some_and(|members| {
                members
                    .keys()
                    .any(|member| normalize_email(member.as_str()).as_deref() == Some(&email))
            })
        {
            return Err(HubError::AlreadyMember);
        }
        let now = self.invitation_now();
        if let Some(existing) = self.state.invitations.values().find(|invitation| {
            invitation.organization == *organization
                && invitation.email == email
                && invitation.status == InvitationStatus::Pending
                && invitation.expires_at_epoch_seconds > now
        }) {
            return Ok(summary(existing));
        }
        let entitlements = self
            .state
            .invitation_entitlements
            .get(organization)
            .cloned()
            .unwrap_or(InvitationEntitlements {
                enabled: true,
                seat_limit: None,
            });
        if !entitlements.enabled {
            return Err(HubError::InvitationsDisabled);
        }
        if let Some(limit) = entitlements.seat_limit {
            let members = self
                .state
                .memberships
                .get(organization)
                .map_or(0, std::collections::BTreeMap::len);
            let pending = self
                .state
                .invitations
                .values()
                .filter(|invitation| {
                    invitation.organization == *organization
                        && invitation.status == InvitationStatus::Pending
                        && invitation.expires_at_epoch_seconds > now
                })
                .count();
            if members + pending >= limit {
                return Err(HubError::SeatLimitReached);
            }
        }
        self.state.next_invitation_sequence += 1;
        let sequence = self.state.next_invitation_sequence;
        let id = format!("invitation-{sequence}");
        let invitation = StoredInvitation {
            id: id.clone(),
            organization: organization.clone(),
            email,
            role,
            status: InvitationStatus::Pending,
            expires_at_epoch_seconds: now + INVITATION_LIFETIME_SECONDS,
            created_sequence: sequence,
        };
        let result = summary(&invitation);
        self.state.invitations.insert(id, invitation);
        self.persist()?;
        Ok(result)
    }

    pub fn pending_invitations(
        &self,
        actor: &AccountId,
        organization: &OrganizationId,
    ) -> Result<Vec<InvitationSummary>, HubError> {
        if !self.authorize(actor, organization)?.can_manage_resources() {
            return Err(HubError::InvitationManagementRequired);
        }
        let now = self.invitation_now();
        let mut invitations = self
            .state
            .invitations
            .values()
            .filter(|invitation| {
                invitation.organization == *organization
                    && invitation.status == InvitationStatus::Pending
                    && invitation.expires_at_epoch_seconds > now
            })
            .collect::<Vec<_>>();
        invitations.sort_by_key(|invitation| invitation.created_sequence);
        Ok(invitations.into_iter().map(summary).collect())
    }

    pub fn cancel_invitation(
        &mut self,
        actor: &AccountId,
        organization: &OrganizationId,
        invitation_id: &str,
    ) -> Result<(), HubError> {
        if !self.authorize(actor, organization)?.can_manage_resources() {
            return Err(HubError::InvitationManagementRequired);
        }
        let invitation = self
            .state
            .invitations
            .get_mut(invitation_id)
            .filter(|invitation| {
                invitation.organization == *organization
                    && invitation.status == InvitationStatus::Pending
            })
            .ok_or(HubError::InvitationUnavailable)?;
        invitation.status = InvitationStatus::Canceled;
        self.persist()
    }

    pub fn accept_invitation(
        &mut self,
        account: &AccountId,
        invitation_id: &str,
    ) -> Result<OrganizationId, HubError> {
        let normalized_account =
            normalize_email(account.as_str()).ok_or(HubError::InvitationUnavailable)?;
        let now = self.invitation_now();
        let invitation = self
            .state
            .invitations
            .get(invitation_id)
            .filter(|invitation| {
                invitation.status == InvitationStatus::Pending
                    && invitation.expires_at_epoch_seconds > now
                    && invitation.email == normalized_account
            })
            .cloned()
            .ok_or(HubError::InvitationUnavailable)?;
        self.state
            .accounts
            .entry(account.clone())
            .or_insert(Account {
                password_fingerprint: fingerprint("invitation-account"),
                must_change_password: false,
            });
        self.state
            .memberships
            .entry(invitation.organization.clone())
            .or_default()
            .entry(account.clone())
            .or_insert_with(|| invitation.role.into());
        self.state
            .invitations
            .get_mut(invitation_id)
            .ok_or(HubError::InvitationUnavailable)?
            .status = InvitationStatus::Accepted;
        self.persist()?;
        Ok(invitation.organization)
    }

    fn invitation_now(&self) -> u64 {
        self.invitation_now_override
            .unwrap_or_else(now_epoch_seconds)
    }
}

fn summary(invitation: &StoredInvitation) -> InvitationSummary {
    InvitationSummary {
        id: invitation.id.clone(),
        email: invitation.email.clone(),
        role: invitation.role,
        expires_at_epoch_seconds: invitation.expires_at_epoch_seconds,
        link: format!("{INVITATION_BASE_URL}/?invitation={}", invitation.id),
    }
}

fn normalize_email(email: &str) -> Option<String> {
    let email = email.trim().to_ascii_lowercase();
    let (local, domain) = email.split_once('@')?;
    (!local.is_empty() && domain.contains('.') && !domain.starts_with('.')).then_some(email)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
