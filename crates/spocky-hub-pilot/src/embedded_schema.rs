// Generated from pinned Hub Drizzle snapshot 0048 and journal.
// Regenerate with scripts/phase2/hub-embedded-schema-generate.mjs.

#![allow(clippy::unreadable_literal)]

pub const BASELINE_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "account" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "account_id" TEXT NOT NULL,
  "provider_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "access_token" TEXT,
  "refresh_token" TEXT,
  "id_token" TEXT,
  "access_token_expires_at" TEXT,
  "refresh_token_expires_at" TEXT,
  "scope" TEXT,
  "password" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "account_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "user" ("id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "agent_executions" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "agent_session_action" TEXT,
  "agent_session_id" TEXT,
  "organization_id" TEXT NOT NULL,
  "project_id" TEXT NOT NULL,
  "machine_id" TEXT,
  "status" TEXT NOT NULL,
  "started_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "completed_at" TEXT,
  "completed_by_agent_at" TEXT,
  "deadline_at" TEXT,
  "idle_deadline_at" TEXT,
  "result" TEXT,
  "trigger_context" TEXT,
  "output_context" TEXT,
  "reaction_state" TEXT,
  "configuration_revision_id" TEXT NOT NULL,
  "completion_token_hash" TEXT,
  "reply_claimed_at" TEXT,
  "reply_claim_count" INTEGER NOT NULL DEFAULT 0,
  "output_emissions" TEXT NOT NULL DEFAULT '{}',
  "output_delivery_attempts" TEXT NOT NULL DEFAULT '{}',
  "launch_intent" TEXT,
  "daemon_id" TEXT,
  "daemon_agent_id" TEXT,
  "workflow_step_run_id" TEXT,
  "hub_action" TEXT,
  "hub_action_completed_at" TEXT,
  "hub_action_ready_at" TEXT,
  "hub_action_acknowledgements" TEXT NOT NULL DEFAULT '{"terminal_at":null,"idle_at":null,"finish_execution_call":null}',
  CONSTRAINT "agent_executions_agent_session_id_agent_sessions_id_fk" FOREIGN KEY ("agent_session_id") REFERENCES "agent_sessions" ("id"),
  CONSTRAINT "agent_executions_project_organization_fk" FOREIGN KEY ("project_id", "organization_id") REFERENCES "projects" ("id", "organization_id"),
  CONSTRAINT "agent_executions_revision_project_organization_fk" FOREIGN KEY ("configuration_revision_id", "project_id", "organization_id") REFERENCES "project_configuration_revisions" ("id", "project_id", "organization_id"),
  CONSTRAINT "agent_executions_machine_organization_fk" FOREIGN KEY ("machine_id", "organization_id") REFERENCES "machines" ("id", "org_id"),
  CONSTRAINT "agent_executions_daemon_organization_fk" FOREIGN KEY ("daemon_id", "organization_id") REFERENCES "daemons" ("id", "organization_id"),
  CONSTRAINT "agent_executions_workflow_step_run_fk" FOREIGN KEY ("workflow_step_run_id") REFERENCES "workflow_step_runs" ("id"),
  CONSTRAINT "agent_executions_hub_action_check" CHECK ("hub_action" is null or "hub_action" in ('interrupt', 'archive'))
);
CREATE TABLE IF NOT EXISTS "agent_sessions" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "project_id" TEXT NOT NULL,
  "continuation_key" TEXT,
  "data" TEXT NOT NULL,
  CONSTRAINT "agent_sessions_project_id_projects_id_fk" FOREIGN KEY ("project_id") REFERENCES "projects" ("id")
);
CREATE TABLE IF NOT EXISTS "attachment_capabilities" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "provider_event_receipt_id" TEXT NOT NULL,
  "organization_id" TEXT NOT NULL,
  "connection_id" TEXT NOT NULL,
  "provider" TEXT NOT NULL,
  "source_id" TEXT NOT NULL,
  "locator" TEXT NOT NULL,
  "filename" TEXT NOT NULL,
  "content_type" TEXT,
  "byte_size" INTEGER,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "attachment_capabilities_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "attachment_capabilities_receipt_organization_fk" FOREIGN KEY ("provider_event_receipt_id", "organization_id") REFERENCES "provider_event_receipts" ("id", "organization_id") ON DELETE CASCADE,
  CONSTRAINT "attachment_capabilities_provider_check" CHECK ("provider" in ('slack', 'discord'))
);
CREATE TABLE IF NOT EXISTS "audit_events" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "project_id" TEXT,
  "actor_kind" TEXT NOT NULL,
  "actor_identity" TEXT NOT NULL,
  "action" TEXT NOT NULL,
  "subject_type" TEXT NOT NULL,
  "subject_id" TEXT NOT NULL,
  "evidence" TEXT NOT NULL,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "audit_events_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "audit_events_project_id_projects_id_fk" FOREIGN KEY ("project_id") REFERENCES "projects" ("id") ON DELETE CASCADE,
  CONSTRAINT "audit_events_project_organization_fk" FOREIGN KEY ("project_id", "organization_id") REFERENCES "projects" ("id", "organization_id") ON DELETE CASCADE,
  CONSTRAINT "audit_events_actor_kind_check" CHECK ("actor_kind" in ('user', 'github', 'system'))
);
CREATE TABLE IF NOT EXISTS "billing_plan_prices" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "plan_id" TEXT NOT NULL,
  "lookup_key" TEXT NOT NULL,
  "interval" TEXT NOT NULL,
  "unit_amount" INTEGER NOT NULL,
  "currency" TEXT NOT NULL,
  "active" INTEGER NOT NULL,
  CONSTRAINT "billing_plan_prices_plan_id_billing_plans_id_fk" FOREIGN KEY ("plan_id") REFERENCES "billing_plans" ("id") ON DELETE CASCADE,
  CONSTRAINT "billing_plan_prices_interval_check" CHECK ("interval" in ('monthly', 'annual'))
);
CREATE TABLE IF NOT EXISTS "billing_plans" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "slug" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "template" TEXT NOT NULL,
  "template_hash" TEXT NOT NULL,
  "marketing" TEXT NOT NULL,
  "active" INTEGER NOT NULL,
  "synced_at" TEXT NOT NULL,
  CONSTRAINT "billing_plans_slug_unique" UNIQUE ("slug")
);
CREATE TABLE IF NOT EXISTS "cli_authorizations" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "device_verifier" TEXT NOT NULL,
  "user_code_verifier" TEXT NOT NULL,
  "fingerprint_verifier" TEXT NOT NULL,
  "status" TEXT NOT NULL,
  "poll_interval_seconds" INTEGER NOT NULL,
  "next_poll_at" TEXT NOT NULL,
  "approved_organization_id" TEXT,
  "approved_by_user_id" TEXT,
  "decided_at" TEXT,
  "credential_id" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "expires_at" TEXT NOT NULL,
  CONSTRAINT "cli_authorizations_device_verifier_unique" UNIQUE ("device_verifier"),
  CONSTRAINT "cli_authorizations_user_code_verifier_unique" UNIQUE ("user_code_verifier"),
  CONSTRAINT "cli_authorizations_credential_id_unique" UNIQUE ("credential_id"),
  CONSTRAINT "cli_authorizations_status_check" CHECK ("status" in ('pending', 'approved', 'denied', 'expired', 'disclosed')),
  CONSTRAINT "cli_authorizations_poll_interval_check" CHECK ("poll_interval_seconds" >= 5)
);
CREATE TABLE IF NOT EXISTS "configuration_sync_attempts" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "project_id" TEXT NOT NULL,
  "github_connection_id" TEXT,
  "github_repository_id" INTEGER,
  "webhook_delivery_id" TEXT,
  "commit_sha" TEXT,
  "outcome" TEXT NOT NULL,
  "evidence" TEXT NOT NULL,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "configuration_sync_attempts_project_organization_fk" FOREIGN KEY ("project_id", "organization_id") REFERENCES "projects" ("id", "organization_id") ON DELETE CASCADE,
  CONSTRAINT "configuration_sync_attempts_github_connection_organization_fk" FOREIGN KEY ("github_connection_id", "organization_id") REFERENCES "github_connections" ("id", "organization_id") ON DELETE SET NULL
);
CREATE TABLE IF NOT EXISTS "daemon_enrollment_tokens" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "verifier" TEXT NOT NULL,
  "organization_id" TEXT,
  "issued_by_api_key_id" TEXT,
  "issued_by_cli_credential_id" TEXT,
  "expires_at" TEXT NOT NULL,
  "consumed_at" TEXT,
  CONSTRAINT "daemon_enrollment_tokens_verifier_unique" UNIQUE ("verifier"),
  CONSTRAINT "daemon_enrollment_tokens_issued_by_cli_credential_id_organization_cli_credentials_id_fk" FOREIGN KEY ("issued_by_cli_credential_id") REFERENCES "organization_cli_credentials" ("id") ON DELETE SET NULL
);
CREATE TABLE IF NOT EXISTS "daemons" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "idempotency_key" TEXT NOT NULL,
  "enrollment_verifier" TEXT NOT NULL,
  "slug" TEXT NOT NULL,
  "machine_id" TEXT NOT NULL,
  "organization_id" TEXT NOT NULL,
  "server_id" TEXT NOT NULL,
  "daemon_public_key" TEXT NOT NULL,
  "credential_verifier" TEXT NOT NULL,
  "scopes" TEXT NOT NULL,
  "registered_by_api_key_id" TEXT,
  "registered_by_cli_credential_id" TEXT,
  "status" TEXT NOT NULL,
  "presence" TEXT NOT NULL DEFAULT 'offline',
  "connected_at" TEXT,
  "disconnected_at" TEXT,
  "last_seen_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "daemons_idempotency_key_unique" UNIQUE ("idempotency_key"),
  CONSTRAINT "daemons_registered_by_cli_credential_id_organization_cli_credentials_id_fk" FOREIGN KEY ("registered_by_cli_credential_id") REFERENCES "organization_cli_credentials" ("id") ON DELETE SET NULL,
  CONSTRAINT "daemons_machine_organization_fk" FOREIGN KEY ("machine_id", "organization_id") REFERENCES "machines" ("id", "org_id"),
  CONSTRAINT "daemons_status_check" CHECK ("status" in ('active', 'revoked')),
  CONSTRAINT "daemons_presence_check" CHECK ("presence" in ('offline', 'connected'))
);
CREATE TABLE IF NOT EXISTS "discord_connections" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "guild_id" TEXT NOT NULL,
  "provider_application_id" TEXT,
  "slug" TEXT NOT NULL,
  "guild_name" TEXT NOT NULL,
  "connected_by_user_id" TEXT,
  "connected_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "discord_connections_guild_id_unique" UNIQUE ("guild_id"),
  CONSTRAINT "discord_connections_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "discord_connections_connected_by_user_id_user_id_fk" FOREIGN KEY ("connected_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL
);
CREATE TABLE IF NOT EXISTS "entitlement_changes" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "actor" TEXT,
  "source" TEXT NOT NULL,
  "before" TEXT,
  "after" TEXT NOT NULL,
  "reason" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "entitlement_changes_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "entitlement_changes_source_check" CHECK ("source" in ('provisioning', 'plan_stamp', 'override'))
);
CREATE TABLE IF NOT EXISTS "execution_authorities" (
  "execution_id" TEXT PRIMARY KEY NOT NULL,
  "data" TEXT NOT NULL,
  CONSTRAINT "execution_authorities_execution_id_agent_executions_id_fk" FOREIGN KEY ("execution_id") REFERENCES "agent_executions" ("id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "execution_credential_leases" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "execution_id" TEXT NOT NULL,
  "data" TEXT NOT NULL,
  CONSTRAINT "execution_credential_leases_execution_id_agent_executions_id_fk" FOREIGN KEY ("execution_id") REFERENCES "agent_executions" ("id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "github_connections" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "installation_id" INTEGER NOT NULL,
  "provider_application_id" TEXT,
  "slug" TEXT NOT NULL,
  "account_id" TEXT NOT NULL,
  "account_login" TEXT NOT NULL,
  "account_type" TEXT NOT NULL,
  "status" TEXT NOT NULL,
  "connected_by_user_id" TEXT,
  "connected_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "suspended_at" TEXT,
  CONSTRAINT "github_connections_installation_id_unique" UNIQUE ("installation_id"),
  CONSTRAINT "github_connections_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "github_connections_connected_by_user_id_user_id_fk" FOREIGN KEY ("connected_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL,
  CONSTRAINT "github_connections_status_check" CHECK ("status" in ('active', 'suspended'))
);
CREATE TABLE IF NOT EXISTS "github_repositories" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "connection_id" TEXT NOT NULL,
  "repository_id" INTEGER NOT NULL,
  "full_name" TEXT NOT NULL,
  "default_branch" TEXT NOT NULL,
  "discovered_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "github_repositories_connection_organization_fk" FOREIGN KEY ("connection_id", "organization_id") REFERENCES "github_connections" ("id", "organization_id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "instance_bootstrap" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT,
  "owner_user_id" TEXT,
  "completed_at" TEXT,
  "app_onboarding_completed_at" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "instance_bootstrap_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE RESTRICT,
  CONSTRAINT "instance_bootstrap_owner_user_id_user_id_fk" FOREIGN KEY ("owner_user_id") REFERENCES "user" ("id") ON DELETE RESTRICT,
  CONSTRAINT "instance_bootstrap_completion_check" CHECK ("completed_at" is null or ("organization_id" is not null and "owner_user_id" is not null))
);
CREATE TABLE IF NOT EXISTS "invitation" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "email" TEXT NOT NULL,
  "role" TEXT NOT NULL,
  "status" TEXT NOT NULL,
  "expires_at" TEXT NOT NULL,
  "inviter_id" TEXT NOT NULL,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "invitation_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "invitation_inviter_id_user_id_fk" FOREIGN KEY ("inviter_id") REFERENCES "user" ("id") ON DELETE CASCADE,
  CONSTRAINT "invitations_role_check" CHECK ("role" in ('admin', 'member')),
  CONSTRAINT "invitations_status_check" CHECK ("status" in ('pending', 'accepted', 'rejected', 'canceled'))
);
CREATE TABLE IF NOT EXISTS "linear_connections" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "linear_organization_id" TEXT NOT NULL,
  "provider_application_id" TEXT,
  "slug" TEXT NOT NULL,
  "linear_organization_name" TEXT NOT NULL,
  "app_user_id" TEXT NOT NULL,
  "access_token" TEXT NOT NULL,
  "refresh_token" TEXT,
  "access_token_expires_at" TEXT,
  "scopes" TEXT NOT NULL DEFAULT '[]',
  "connected_by_user_id" TEXT,
  "connected_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "linear_connections_linear_organization_id_unique" UNIQUE ("linear_organization_id"),
  CONSTRAINT "linear_connections_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "linear_connections_connected_by_user_id_user_id_fk" FOREIGN KEY ("connected_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL
);
CREATE TABLE IF NOT EXISTS "machines" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "org_id" TEXT NOT NULL,
  "source" TEXT NOT NULL,
  "status" TEXT NOT NULL,
  "started_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "terminated_at" TEXT,
  "shutdown_reason" TEXT,
  "trigger_name" TEXT,
  "trigger_context" TEXT,
  "specs" TEXT
);
CREATE TABLE IF NOT EXISTS "member" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "role" TEXT NOT NULL,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "member_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "member_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "user" ("id") ON DELETE CASCADE,
  CONSTRAINT "members_role_check" CHECK ("role" in ('owner', 'admin', 'member'))
);
CREATE TABLE IF NOT EXISTS "organization" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "name" TEXT NOT NULL,
  "slug" TEXT NOT NULL,
  "logo" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "metadata" TEXT,
  CONSTRAINT "organization_slug_unique" UNIQUE ("slug")
);
CREATE TABLE IF NOT EXISTS "organization_api_keys" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "prefix" TEXT NOT NULL,
  "verifier" TEXT NOT NULL,
  "scopes" TEXT NOT NULL,
  "created_by_user_id" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "last_used_at" TEXT,
  "revoked_at" TEXT,
  CONSTRAINT "organization_api_keys_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "organization_api_keys_created_by_user_id_user_id_fk" FOREIGN KEY ("created_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL,
  CONSTRAINT "organization_api_keys_scopes_check" CHECK (json_valid(scopes) AND json_array_length(scopes) > 0)
);
CREATE TABLE IF NOT EXISTS "organization_billing_customers" (
  "organization_id" TEXT PRIMARY KEY NOT NULL,
  "stripe_customer_id" TEXT NOT NULL,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "organization_billing_customers_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "organization_cli_credentials" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "prefix" TEXT NOT NULL,
  "verifier" TEXT NOT NULL,
  "created_by_user_id" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "last_used_at" TEXT,
  "revoked_at" TEXT,
  CONSTRAINT "organization_cli_credentials_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "organization_cli_credentials_created_by_user_id_user_id_fk" FOREIGN KEY ("created_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL
);
CREATE TABLE IF NOT EXISTS "organization_connection_attempts" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "provider" TEXT NOT NULL,
  "phase" TEXT NOT NULL,
  "state_verifier" TEXT NOT NULL,
  "organization_id" TEXT NOT NULL,
  "return_route" TEXT NOT NULL,
  "user_id" TEXT NOT NULL,
  "session_id" TEXT NOT NULL,
  "candidate_external_id" TEXT,
  "pkce_verifier" TEXT,
  "configuration_version" INTEGER NOT NULL,
  "provider_application_id" TEXT,
  "callback_origin" TEXT NOT NULL,
  "configuration_snapshot" TEXT NOT NULL,
  "expected_configuration_version" INTEGER,
  "activate_configuration" INTEGER NOT NULL DEFAULT 0,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "expires_at" TEXT NOT NULL,
  "consumed_at" TEXT,
  CONSTRAINT "organization_connection_attempts_state_verifier_unique" UNIQUE ("state_verifier"),
  CONSTRAINT "organization_connection_attempts_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "organization_connection_attempts_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "user" ("id") ON DELETE CASCADE,
  CONSTRAINT "organization_connection_attempts_session_id_session_id_fk" FOREIGN KEY ("session_id") REFERENCES "session" ("id") ON DELETE CASCADE,
  CONSTRAINT "organization_connection_attempts_provider_check" CHECK ("provider" in ('github', 'discord', 'slack', 'linear')),
  CONSTRAINT "organization_connection_attempts_phase_check" CHECK ("phase" in ('github_setup', 'github_user_authorization', 'discord_authorization', 'slack_authorization', 'linear_authorization')),
  CONSTRAINT "organization_connection_attempts_shape_check" CHECK (("phase" = 'github_setup' and "provider" = 'github' and "candidate_external_id" is null and "pkce_verifier" is null)
        or ("phase" = 'github_user_authorization' and "provider" = 'github' and "candidate_external_id" is not null and ("pkce_verifier" is not null or "consumed_at" is not null))
        or ("phase" = 'discord_authorization' and "provider" = 'discord' and "candidate_external_id" is null and "pkce_verifier" is null)
        or ("phase" = 'slack_authorization' and "provider" = 'slack' and "candidate_external_id" is null and "pkce_verifier" is null)
        or ("phase" = 'linear_authorization' and "provider" = 'linear' and "candidate_external_id" is null and "pkce_verifier" is null))
);
CREATE TABLE IF NOT EXISTS "organization_entitlements" (
  "organization_id" TEXT PRIMARY KEY NOT NULL,
  "granted" TEXT NOT NULL,
  "overrides" TEXT NOT NULL DEFAULT '{}',
  "plan_id" TEXT,
  "plan_version" TEXT,
  "stamped_at" TEXT NOT NULL,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "organization_entitlements_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "organization_trigger_revisions" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "trigger_id" TEXT NOT NULL,
  "organization_id" TEXT NOT NULL,
  "version" INTEGER NOT NULL,
  "yaml" TEXT NOT NULL,
  "normalized_configuration" TEXT NOT NULL,
  "content_hash" TEXT NOT NULL,
  "source_kind" TEXT NOT NULL,
  "source_evidence" TEXT NOT NULL,
  "created_by_user_id" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "organization_trigger_revisions_created_by_user_id_user_id_fk" FOREIGN KEY ("created_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL,
  CONSTRAINT "organization_trigger_revisions_trigger_organization_fk" FOREIGN KEY ("trigger_id", "organization_id") REFERENCES "organization_triggers" ("id", "organization_id") ON DELETE CASCADE,
  CONSTRAINT "organization_trigger_revisions_source_kind_check" CHECK ("source_kind" in ('manual', 'github', 'project_migration'))
);
CREATE TABLE IF NOT EXISTS "organization_trigger_routes" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "trigger_id" TEXT NOT NULL,
  "trigger_revision_id" TEXT NOT NULL,
  "provider" TEXT NOT NULL,
  "connection_id" TEXT NOT NULL,
  "resource_id" TEXT,
  "configured_event_name" TEXT NOT NULL,
  CONSTRAINT "organization_trigger_routes_trigger_organization_fk" FOREIGN KEY ("trigger_id", "organization_id") REFERENCES "organization_triggers" ("id", "organization_id") ON DELETE CASCADE,
  CONSTRAINT "organization_trigger_routes_revision_trigger_organization_fk" FOREIGN KEY ("trigger_revision_id", "trigger_id", "organization_id") REFERENCES "organization_trigger_revisions" ("id", "trigger_id", "organization_id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "organization_triggers" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "enabled" INTEGER NOT NULL DEFAULT 1,
  "format" TEXT NOT NULL,
  "runtime_project_id" TEXT,
  "active_revision_id" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "organization_triggers_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "organization_triggers_runtime_project_id_projects_id_fk" FOREIGN KEY ("runtime_project_id") REFERENCES "projects" ("id"),
  CONSTRAINT "organization_triggers_active_revision_id_organization_trigger_revisions_id_fk" FOREIGN KEY ("active_revision_id") REFERENCES "organization_trigger_revisions" ("id"),
  CONSTRAINT "organization_triggers_format_check" CHECK ("format" in ('single_run', 'legacy_multistep'))
);
CREATE TABLE IF NOT EXISTS "organization_usage" (
  "organization_id" TEXT NOT NULL,
  "meter" TEXT NOT NULL,
  "period_start" TEXT NOT NULL,
  "used" INTEGER NOT NULL DEFAULT 0,
  CONSTRAINT "organization_usage_organization_id_meter_period_start_pk" PRIMARY KEY ("organization_id", "meter", "period_start"),
  CONSTRAINT "organization_usage_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "organization_usage_used_non_negative" CHECK ("used" >= 0)
);
CREATE TABLE IF NOT EXISTS "project_configuration_revisions" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "project_id" TEXT NOT NULL,
  "organization_id" TEXT NOT NULL,
  "version" INTEGER NOT NULL,
  "source_kind" TEXT NOT NULL,
  "source_evidence" TEXT NOT NULL,
  "raw_yaml" TEXT,
  "normalized_configuration" TEXT NOT NULL,
  "validation_errors" TEXT,
  "content_hash" TEXT NOT NULL,
  "github_repository_id" INTEGER,
  "github_repository_full_name" TEXT,
  "github_commit_sha" TEXT,
  "github_commit_url" TEXT,
  "github_ref" TEXT,
  "github_webhook_delivery_id" TEXT,
  "github_sender" TEXT,
  "github_author" TEXT,
  "github_committer" TEXT,
  "created_by_user_id" TEXT,
  "received_at" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "validated_at" TEXT,
  CONSTRAINT "project_configuration_revisions_created_by_user_id_user_id_fk" FOREIGN KEY ("created_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL,
  CONSTRAINT "project_configuration_revisions_project_organization_fk" FOREIGN KEY ("project_id", "organization_id") REFERENCES "projects" ("id", "organization_id") ON DELETE CASCADE,
  CONSTRAINT "project_configuration_revisions_source_kind_check" CHECK ("source_kind" in ('github', 'manual'))
);
CREATE TABLE IF NOT EXISTS "project_configuration_sources" (
  "organization_id" TEXT NOT NULL,
  "project_id" TEXT PRIMARY KEY NOT NULL,
  "kind" TEXT NOT NULL,
  "github_connection_id" TEXT,
  "github_repository_id" INTEGER,
  "github_repository_full_name" TEXT,
  "github_default_branch" TEXT,
  "automatic_deployment_enabled" INTEGER NOT NULL DEFAULT 0,
  "selected_by_user_id" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "project_configuration_sources_selected_by_user_id_user_id_fk" FOREIGN KEY ("selected_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL,
  CONSTRAINT "project_configuration_sources_project_organization_fk" FOREIGN KEY ("project_id", "organization_id") REFERENCES "projects" ("id", "organization_id") ON DELETE CASCADE,
  CONSTRAINT "project_configuration_sources_github_connection_organization_fk" FOREIGN KEY ("github_connection_id", "organization_id") REFERENCES "github_connections" ("id", "organization_id") ON DELETE RESTRICT,
  CONSTRAINT "project_configuration_sources_authority_shape_check" CHECK (("kind" = 'manual' and "github_connection_id" is null and "github_repository_id" is null and not "automatic_deployment_enabled") or ("kind" = 'github' and "github_connection_id" is not null and "github_repository_id" is not null))
);
CREATE TABLE IF NOT EXISTS "project_trigger_migrations" (
  "project_id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "configuration_revision_id" TEXT NOT NULL,
  "migrated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "project_trigger_migrations_project_id_projects_id_fk" FOREIGN KEY ("project_id") REFERENCES "projects" ("id") ON DELETE CASCADE,
  CONSTRAINT "project_trigger_migrations_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "project_trigger_migrations_revision_project_organization_fk" FOREIGN KEY ("configuration_revision_id", "project_id", "organization_id") REFERENCES "project_configuration_revisions" ("id", "project_id", "organization_id")
);
CREATE TABLE IF NOT EXISTS "project_trigger_routes" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "project_id" TEXT NOT NULL,
  "configuration_revision_id" TEXT NOT NULL,
  "provider" TEXT NOT NULL,
  "connection_id" TEXT NOT NULL,
  "resource_id" TEXT,
  "trigger_name" TEXT NOT NULL,
  CONSTRAINT "project_trigger_routes_project_organization_fk" FOREIGN KEY ("project_id", "organization_id") REFERENCES "projects" ("id", "organization_id") ON DELETE CASCADE,
  CONSTRAINT "project_trigger_routes_revision_project_organization_fk" FOREIGN KEY ("configuration_revision_id", "project_id", "organization_id") REFERENCES "project_configuration_revisions" ("id", "project_id", "organization_id") ON DELETE CASCADE,
  CONSTRAINT "project_trigger_routes_provider_check" CHECK ("provider" in ('github', 'slack', 'discord', 'linear'))
);
CREATE TABLE IF NOT EXISTS "projects" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "name" TEXT NOT NULL,
  "slug" TEXT NOT NULL,
  "status" TEXT NOT NULL DEFAULT 'active',
  "created_by_user_id" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "archived_at" TEXT,
  "active_configuration_revision_id" TEXT,
  CONSTRAINT "projects_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "projects_created_by_user_id_user_id_fk" FOREIGN KEY ("created_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL,
  CONSTRAINT "projects_active_configuration_revision_id_project_configuration_revisions_id_fk" FOREIGN KEY ("active_configuration_revision_id") REFERENCES "project_configuration_revisions" ("id"),
  CONSTRAINT "projects_status_check" CHECK ("status" in ('active', 'archived')),
  CONSTRAINT "projects_archive_shape_check" CHECK (("status" = 'active' and "archived_at" is null) or ("status" = 'archived' and "archived_at" is not null and "active_configuration_revision_id" is null))
);
CREATE TABLE IF NOT EXISTS "provider_event_receipts" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "provider" TEXT NOT NULL,
  "connection_id" TEXT,
  "resource_id" TEXT,
  "delivery_id" TEXT NOT NULL,
  "signature_hash" TEXT,
  "provider_application_id" TEXT,
  "provider_configuration_version" INTEGER,
  "source" TEXT NOT NULL,
  "repo" TEXT,
  "payload" TEXT NOT NULL,
  "received_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "dropped_reason" TEXT,
  "accepted_routes" TEXT,
  CONSTRAINT "provider_event_receipts_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "provider_event_receipts_provider_check" CHECK ("provider" in ('github', 'slack', 'discord', 'linear', 'manual', 'schedule'))
);
CREATE TABLE IF NOT EXISTS "runtime_configuration" (
  "singleton" INTEGER PRIMARY KEY NOT NULL DEFAULT 1,
  "auth_secret" TEXT NOT NULL,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "runtime_configuration_singleton_check" CHECK ("singleton")
);
CREATE TABLE IF NOT EXISTS "runtime_provider_activation" (
  "provider" TEXT PRIMARY KEY NOT NULL,
  "provider_application_id" TEXT NOT NULL,
  "configuration_version" INTEGER NOT NULL,
  "activated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "runtime_provider_activation_provider_check" CHECK ("provider" in ('github', 'slack', 'discord', 'linear')),
  CONSTRAINT "runtime_provider_activation_version_check" CHECK ("configuration_version" >= 0)
);
CREATE TABLE IF NOT EXISTS "runtime_provider_configuration" (
  "provider" TEXT PRIMARY KEY NOT NULL,
  "configuration" TEXT NOT NULL,
  "verified_external_identity" TEXT NOT NULL,
  "version" INTEGER NOT NULL DEFAULT 1,
  "verified_at" TEXT NOT NULL,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_by_user_id" TEXT,
  CONSTRAINT "runtime_provider_configuration_updated_by_user_id_user_id_fk" FOREIGN KEY ("updated_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL,
  CONSTRAINT "runtime_provider_configuration_provider_check" CHECK ("provider" in ('github', 'slack', 'discord', 'linear')),
  CONSTRAINT "runtime_provider_configuration_version_check" CHECK ("version" > 0)
);
CREATE TABLE IF NOT EXISTS "session" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "expires_at" TEXT NOT NULL,
  "token" TEXT NOT NULL,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "ip_address" TEXT,
  "user_agent" TEXT,
  "user_id" TEXT NOT NULL,
  "active_organization_id" TEXT,
  CONSTRAINT "session_token_unique" UNIQUE ("token"),
  CONSTRAINT "session_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "user" ("id") ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS "slack_connections" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "team_id" TEXT NOT NULL,
  "provider_application_id" TEXT,
  "slug" TEXT NOT NULL,
  "team_name" TEXT NOT NULL,
  "bot_user_id" TEXT NOT NULL,
  "bot_access_token" TEXT NOT NULL,
  "scopes" TEXT NOT NULL DEFAULT '[]',
  "connected_by_user_id" TEXT,
  "connected_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CONSTRAINT "slack_connections_team_id_unique" UNIQUE ("team_id"),
  CONSTRAINT "slack_connections_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "slack_connections_connected_by_user_id_user_id_fk" FOREIGN KEY ("connected_by_user_id") REFERENCES "user" ("id") ON DELETE SET NULL
);
CREATE TABLE IF NOT EXISTS "trigger_runs" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "organization_id" TEXT NOT NULL,
  "project_id" TEXT NOT NULL,
  "configuration_revision_id" TEXT NOT NULL,
  "provider_event_receipt_id" TEXT NOT NULL,
  "configured_trigger_name" TEXT NOT NULL,
  "conversation" TEXT,
  "outcome" TEXT NOT NULL DEFAULT 'accepted',
  "status" TEXT NOT NULL,
  "prompt" TEXT NOT NULL,
  "inputs" TEXT NOT NULL DEFAULT '{}',
  "values" TEXT NOT NULL DEFAULT '{}',
  "trigger_context" TEXT NOT NULL DEFAULT '{}',
  "output_context" TEXT NOT NULL DEFAULT '{}',
  "deadline_at" TEXT,
  "deadline_kind" TEXT,
  "failure_reason" TEXT,
  "reaction_state" TEXT,
  "terminal_notification_pending_at" TEXT,
  "terminal_notification_delivered_at" TEXT,
  "terminal_notification_lease_expires_at" TEXT,
  "rejection" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "completed_at" TEXT,
  CONSTRAINT "trigger_runs_organization_fk" FOREIGN KEY ("organization_id") REFERENCES "organization" ("id") ON DELETE CASCADE,
  CONSTRAINT "trigger_runs_project_organization_fk" FOREIGN KEY ("project_id", "organization_id") REFERENCES "projects" ("id", "organization_id") ON DELETE CASCADE,
  CONSTRAINT "trigger_runs_revision_project_organization_fk" FOREIGN KEY ("configuration_revision_id", "project_id", "organization_id") REFERENCES "project_configuration_revisions" ("id", "project_id", "organization_id"),
  CONSTRAINT "trigger_runs_receipt_organization_fk" FOREIGN KEY ("provider_event_receipt_id", "organization_id") REFERENCES "provider_event_receipts" ("id", "organization_id"),
  CONSTRAINT "trigger_runs_status_check" CHECK ("status" in ('running', 'succeeded', 'failed', 'timed_out', 'rejected')),
  CONSTRAINT "trigger_runs_outcome_check" CHECK (("outcome" = 'accepted' and "status" <> 'rejected' and "rejection" is null)
        or ("outcome" = 'rejected' and "status" = 'rejected' and "rejection" is not null)),
  CONSTRAINT "trigger_runs_deadline_kind_check" CHECK ("deadline_kind" is null or "deadline_kind" in ('step_hard', 'step_idle', 'whole_run')),
  CONSTRAINT "trigger_runs_deadline_shape_check" CHECK (("outcome" = 'accepted' and "deadline_at" is not null)
        or ("outcome" = 'rejected' and "deadline_at" is null))
);
CREATE TABLE IF NOT EXISTS "trigger_schedules" (
  "trigger_id" TEXT PRIMARY KEY NOT NULL,
  "recurrence" TEXT,
  "next_at" TEXT,
  "active_run_id" TEXT,
  CONSTRAINT "trigger_schedules_trigger_id_organization_triggers_id_fk" FOREIGN KEY ("trigger_id") REFERENCES "organization_triggers" ("id") ON DELETE CASCADE,
  CONSTRAINT "trigger_schedules_active_run_id_trigger_runs_id_fk" FOREIGN KEY ("active_run_id") REFERENCES "trigger_runs" ("id") ON DELETE SET NULL
);
CREATE TABLE IF NOT EXISTS "user" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "name" TEXT NOT NULL,
  "email" TEXT NOT NULL,
  "email_verified" INTEGER NOT NULL DEFAULT 0,
  "image" TEXT,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "must_change_password" INTEGER NOT NULL DEFAULT 0,
  "is_instance_operator" INTEGER NOT NULL DEFAULT 0,
  CONSTRAINT "user_email_unique" UNIQUE ("email")
);
CREATE TABLE IF NOT EXISTS "verification" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "identifier" TEXT NOT NULL,
  "value" TEXT NOT NULL,
  "expires_at" TEXT NOT NULL,
  "created_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  "updated_at" TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE IF NOT EXISTS "workflow_step_runs" (
  "id" TEXT PRIMARY KEY NOT NULL,
  "trigger_run_id" TEXT NOT NULL,
  "step_id" TEXT NOT NULL,
  "ordinal" INTEGER NOT NULL,
  "status" TEXT NOT NULL,
  "agent_execution_id" TEXT,
  "output" TEXT,
  "failure_reason" TEXT,
  "deadline_kind" TEXT,
  "deadline_at" TEXT,
  "idle_deadline_at" TEXT,
  "dispatch_intent" TEXT,
  "started_at" TEXT,
  "completed_at" TEXT,
  CONSTRAINT "workflow_step_runs_trigger_run_fk" FOREIGN KEY ("trigger_run_id") REFERENCES "trigger_runs" ("id") ON DELETE CASCADE,
  CONSTRAINT "workflow_step_runs_status_check" CHECK ("status" in ('pending', 'running', 'succeeded', 'skipped', 'failed', 'timed_out')),
  CONSTRAINT "workflow_step_runs_deadline_kind_check" CHECK ("deadline_kind" is null or "deadline_kind" in ('step_hard', 'step_idle', 'whole_run'))
);
CREATE TABLE IF NOT EXISTS "workflow_wakeups" (
  "trigger_run_id" TEXT PRIMARY KEY NOT NULL,
  "available_at" TEXT NOT NULL,
  "lease_expires_at" TEXT,
  CONSTRAINT "workflow_wakeups_trigger_run_fk" FOREIGN KEY ("trigger_run_id") REFERENCES "trigger_runs" ("id") ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS "agent_executions_session_idx" ON "agent_executions" ("agent_session_id");
CREATE INDEX IF NOT EXISTS "agent_executions_machine_id_idx" ON "agent_executions" ("machine_id");
CREATE INDEX IF NOT EXISTS "agent_executions_project_started_at_idx" ON "agent_executions" ("project_id", "started_at" DESC);
CREATE INDEX IF NOT EXISTS "agent_executions_status_idx" ON "agent_executions" ("status");
CREATE UNIQUE INDEX IF NOT EXISTS "agent_sessions_project_key_unique" ON "agent_sessions" ("project_id", "continuation_key");
CREATE UNIQUE INDEX IF NOT EXISTS "attachment_capabilities_receipt_provider_source_unique" ON "attachment_capabilities" ("provider_event_receipt_id", "provider", "source_id");
CREATE INDEX IF NOT EXISTS "attachment_capabilities_receipt_idx" ON "attachment_capabilities" ("provider_event_receipt_id");
CREATE INDEX IF NOT EXISTS "audit_events_organization_created_idx" ON "audit_events" ("organization_id", "created_at" DESC);
CREATE INDEX IF NOT EXISTS "audit_events_project_created_idx" ON "audit_events" ("project_id", "created_at" DESC);
CREATE INDEX IF NOT EXISTS "billing_plan_prices_plan_id_idx" ON "billing_plan_prices" ("plan_id");
CREATE INDEX IF NOT EXISTS "cli_authorizations_fingerprint_idx" ON "cli_authorizations" ("fingerprint_verifier", "expires_at");
CREATE INDEX IF NOT EXISTS "cli_authorizations_status_expiry_idx" ON "cli_authorizations" ("status", "expires_at");
CREATE INDEX IF NOT EXISTS "configuration_sync_attempts_project_created_idx" ON "configuration_sync_attempts" ("project_id", "created_at" DESC);
CREATE UNIQUE INDEX IF NOT EXISTS "daemons_machine_id_unique" ON "daemons" ("machine_id");
CREATE UNIQUE INDEX IF NOT EXISTS "daemons_id_organization_unique" ON "daemons" ("id", "organization_id");
CREATE UNIQUE INDEX IF NOT EXISTS "daemons_organization_slug_unique" ON "daemons" ("organization_id", "slug");
CREATE UNIQUE INDEX IF NOT EXISTS "discord_connections_id_organization_unique" ON "discord_connections" ("id", "organization_id");
CREATE UNIQUE INDEX IF NOT EXISTS "discord_connections_organization_slug_unique" ON "discord_connections" ("organization_id", "slug");
CREATE INDEX IF NOT EXISTS "entitlement_changes_organization_created_idx" ON "entitlement_changes" ("organization_id", "created_at" DESC);
CREATE INDEX IF NOT EXISTS "execution_credential_leases_execution_idx" ON "execution_credential_leases" ("execution_id");
CREATE UNIQUE INDEX IF NOT EXISTS "github_connections_installation_unique" ON "github_connections" ("installation_id");
CREATE UNIQUE INDEX IF NOT EXISTS "github_connections_organization_slug_unique" ON "github_connections" ("organization_id", "slug");
CREATE UNIQUE INDEX IF NOT EXISTS "github_connections_id_organization_unique" ON "github_connections" ("id", "organization_id");
CREATE INDEX IF NOT EXISTS "github_connections_organization_idx" ON "github_connections" ("organization_id");
CREATE UNIQUE INDEX IF NOT EXISTS "github_repositories_connection_repository_unique" ON "github_repositories" ("connection_id", "repository_id");
CREATE INDEX IF NOT EXISTS "github_repositories_organization_idx" ON "github_repositories" ("organization_id");
CREATE INDEX IF NOT EXISTS "invitations_organization_status_idx" ON "invitation" ("organization_id", "status");
CREATE UNIQUE INDEX IF NOT EXISTS "invitations_pending_organization_email_unique" ON "invitation" ("organization_id", lower("email")) WHERE "status" = 'pending';
CREATE UNIQUE INDEX IF NOT EXISTS "linear_connections_id_organization_unique" ON "linear_connections" ("id", "organization_id");
CREATE UNIQUE INDEX IF NOT EXISTS "linear_connections_organization_slug_unique" ON "linear_connections" ("organization_id", "slug");
CREATE UNIQUE INDEX IF NOT EXISTS "linear_connections_organization_external_unique" ON "linear_connections" ("organization_id", "linear_organization_id");
CREATE UNIQUE INDEX IF NOT EXISTS "machines_id_org_id_unique" ON "machines" ("id", "org_id");
CREATE INDEX IF NOT EXISTS "machines_org_id_idx" ON "machines" ("org_id");
CREATE INDEX IF NOT EXISTS "machines_status_idx" ON "machines" ("status");
CREATE UNIQUE INDEX IF NOT EXISTS "members_organization_user_unique" ON "member" ("organization_id", "user_id");
CREATE INDEX IF NOT EXISTS "members_user_id_idx" ON "member" ("user_id");
CREATE INDEX IF NOT EXISTS "members_organization_id_idx" ON "member" ("organization_id");
CREATE UNIQUE INDEX IF NOT EXISTS "organization_api_keys_prefix_unique" ON "organization_api_keys" ("prefix");
CREATE INDEX IF NOT EXISTS "organization_api_keys_organization_created_idx" ON "organization_api_keys" ("organization_id", "created_at" DESC);
CREATE UNIQUE INDEX IF NOT EXISTS "organization_cli_credentials_prefix_unique" ON "organization_cli_credentials" ("prefix");
CREATE INDEX IF NOT EXISTS "organization_cli_credentials_organization_created_idx" ON "organization_cli_credentials" ("organization_id", "created_at" DESC);
CREATE INDEX IF NOT EXISTS "organization_connection_attempts_expiry_idx" ON "organization_connection_attempts" ("expires_at");
CREATE UNIQUE INDEX IF NOT EXISTS "organization_trigger_revisions_trigger_version_unique" ON "organization_trigger_revisions" ("trigger_id", "version");
CREATE UNIQUE INDEX IF NOT EXISTS "organization_trigger_revisions_id_trigger_organization_unique" ON "organization_trigger_revisions" ("id", "trigger_id", "organization_id");
CREATE INDEX IF NOT EXISTS "organization_trigger_revisions_trigger_created_idx" ON "organization_trigger_revisions" ("trigger_id", "created_at" DESC);
CREATE UNIQUE INDEX IF NOT EXISTS "organization_trigger_routes_shape_unique" ON "organization_trigger_routes" ("trigger_id", "trigger_revision_id", "provider", "connection_id", "resource_id", "configured_event_name");
CREATE INDEX IF NOT EXISTS "organization_trigger_routes_resource_idx" ON "organization_trigger_routes" ("organization_id", "provider", "connection_id", "resource_id");
CREATE UNIQUE INDEX IF NOT EXISTS "organization_triggers_organization_name_unique" ON "organization_triggers" ("organization_id", "name");
CREATE UNIQUE INDEX IF NOT EXISTS "organization_triggers_id_organization_unique" ON "organization_triggers" ("id", "organization_id");
CREATE INDEX IF NOT EXISTS "organization_triggers_organization_updated_idx" ON "organization_triggers" ("organization_id", "updated_at" DESC);
CREATE INDEX IF NOT EXISTS "organization_usage_organization_meter_idx" ON "organization_usage" ("organization_id", "meter");
CREATE UNIQUE INDEX IF NOT EXISTS "project_configuration_revisions_project_version_unique" ON "project_configuration_revisions" ("project_id", "version");
CREATE UNIQUE INDEX IF NOT EXISTS "project_configuration_revisions_id_project_organization_unique" ON "project_configuration_revisions" ("id", "project_id", "organization_id");
CREATE INDEX IF NOT EXISTS "project_configuration_revisions_project_created_idx" ON "project_configuration_revisions" ("project_id", "created_at" DESC);
CREATE INDEX IF NOT EXISTS "project_trigger_migrations_organization_idx" ON "project_trigger_migrations" ("organization_id");
CREATE UNIQUE INDEX IF NOT EXISTS "project_trigger_routes_shape_unique" ON "project_trigger_routes" ("project_id", "configuration_revision_id", "provider", "connection_id", "resource_id", "trigger_name");
CREATE INDEX IF NOT EXISTS "project_trigger_routes_resource_idx" ON "project_trigger_routes" ("organization_id", "provider", "connection_id", "resource_id");
CREATE UNIQUE INDEX IF NOT EXISTS "projects_organization_slug_unique" ON "projects" ("organization_id", "slug");
CREATE UNIQUE INDEX IF NOT EXISTS "projects_id_organization_unique" ON "projects" ("id", "organization_id");
CREATE INDEX IF NOT EXISTS "projects_organization_status_idx" ON "projects" ("organization_id", "status");
CREATE UNIQUE INDEX IF NOT EXISTS "provider_event_receipts_id_organization_unique" ON "provider_event_receipts" ("id", "organization_id");
CREATE UNIQUE INDEX IF NOT EXISTS "provider_event_receipts_organization_delivery_unique" ON "provider_event_receipts" ("organization_id", "delivery_id");
CREATE UNIQUE INDEX IF NOT EXISTS "provider_event_receipts_signature_unique" ON "provider_event_receipts" ("signature_hash") WHERE "signature_hash" is not null;
CREATE INDEX IF NOT EXISTS "provider_event_receipts_organization_received_idx" ON "provider_event_receipts" ("organization_id", "received_at" DESC);
CREATE INDEX IF NOT EXISTS "provider_event_receipts_resource_idx" ON "provider_event_receipts" ("organization_id", "provider", "connection_id", "resource_id");
CREATE INDEX IF NOT EXISTS "sessions_active_organization_id_idx" ON "session" ("active_organization_id");
CREATE UNIQUE INDEX IF NOT EXISTS "slack_connections_id_organization_unique" ON "slack_connections" ("id", "organization_id");
CREATE UNIQUE INDEX IF NOT EXISTS "slack_connections_organization_slug_unique" ON "slack_connections" ("organization_id", "slug");
CREATE UNIQUE INDEX IF NOT EXISTS "trigger_runs_receipt_project_configured_unique" ON "trigger_runs" ("provider_event_receipt_id", "project_id", "configured_trigger_name");
CREATE INDEX IF NOT EXISTS "trigger_runs_status_deadline_idx" ON "trigger_runs" ("status", "deadline_at");
CREATE INDEX IF NOT EXISTS "trigger_runs_project_created_idx" ON "trigger_runs" ("project_id", "created_at" DESC);
CREATE INDEX IF NOT EXISTS "trigger_runs_terminal_notification_idx" ON "trigger_runs" ("terminal_notification_delivered_at", "terminal_notification_lease_expires_at");
CREATE INDEX IF NOT EXISTS "trigger_schedules_due_idx" ON "trigger_schedules" ("next_at");
CREATE UNIQUE INDEX IF NOT EXISTS "workflow_step_runs_trigger_ordinal_unique" ON "workflow_step_runs" ("trigger_run_id", "ordinal");
CREATE UNIQUE INDEX IF NOT EXISTS "workflow_step_runs_trigger_step_unique" ON "workflow_step_runs" ("trigger_run_id", "step_id");
CREATE UNIQUE INDEX IF NOT EXISTS "workflow_step_runs_agent_execution_unique" ON "workflow_step_runs" ("agent_execution_id") WHERE "agent_execution_id" is not null;
CREATE INDEX IF NOT EXISTS "workflow_step_runs_trigger_status_idx" ON "workflow_step_runs" ("trigger_run_id", "status");
CREATE INDEX IF NOT EXISTS "workflow_wakeups_available_lease_idx" ON "workflow_wakeups" ("available_at", "lease_expires_at");
"#;

#[allow(dead_code)]
pub const BASELINE_TABLES: &[&str] = &[
    "account",
    "agent_executions",
    "agent_sessions",
    "attachment_capabilities",
    "audit_events",
    "billing_plan_prices",
    "billing_plans",
    "cli_authorizations",
    "configuration_sync_attempts",
    "daemon_enrollment_tokens",
    "daemons",
    "discord_connections",
    "entitlement_changes",
    "execution_authorities",
    "execution_credential_leases",
    "github_connections",
    "github_repositories",
    "instance_bootstrap",
    "invitation",
    "linear_connections",
    "machines",
    "member",
    "organization",
    "organization_api_keys",
    "organization_billing_customers",
    "organization_cli_credentials",
    "organization_connection_attempts",
    "organization_entitlements",
    "organization_trigger_revisions",
    "organization_trigger_routes",
    "organization_triggers",
    "organization_usage",
    "project_configuration_revisions",
    "project_configuration_sources",
    "project_trigger_migrations",
    "project_trigger_routes",
    "projects",
    "provider_event_receipts",
    "runtime_configuration",
    "runtime_provider_activation",
    "runtime_provider_configuration",
    "session",
    "slack_connections",
    "trigger_runs",
    "trigger_schedules",
    "user",
    "verification",
    "workflow_step_runs",
    "workflow_wakeups",
];

#[allow(dead_code)]
pub const BASELINE_CONSTRAINT_NAMES: &[&str] = &[
    "account_user_id_user_id_fk",
    "agent_executions_agent_session_id_agent_sessions_id_fk",
    "agent_executions_daemon_organization_fk",
    "agent_executions_hub_action_check",
    "agent_executions_machine_id_idx",
    "agent_executions_machine_organization_fk",
    "agent_executions_project_organization_fk",
    "agent_executions_project_started_at_idx",
    "agent_executions_revision_project_organization_fk",
    "agent_executions_session_idx",
    "agent_executions_status_idx",
    "agent_executions_workflow_step_run_fk",
    "agent_sessions_project_id_projects_id_fk",
    "agent_sessions_project_key_unique",
    "attachment_capabilities_organization_id_organization_id_fk",
    "attachment_capabilities_provider_check",
    "attachment_capabilities_receipt_idx",
    "attachment_capabilities_receipt_organization_fk",
    "attachment_capabilities_receipt_provider_source_unique",
    "audit_events_actor_kind_check",
    "audit_events_organization_created_idx",
    "audit_events_organization_id_organization_id_fk",
    "audit_events_project_created_idx",
    "audit_events_project_id_projects_id_fk",
    "audit_events_project_organization_fk",
    "billing_plan_prices_interval_check",
    "billing_plan_prices_plan_id_billing_plans_id_fk",
    "billing_plan_prices_plan_id_idx",
    "billing_plans_slug_unique",
    "cli_authorizations_credential_id_unique",
    "cli_authorizations_device_verifier_unique",
    "cli_authorizations_fingerprint_idx",
    "cli_authorizations_poll_interval_check",
    "cli_authorizations_status_check",
    "cli_authorizations_status_expiry_idx",
    "cli_authorizations_user_code_verifier_unique",
    "configuration_sync_attempts_github_connection_organization_fk",
    "configuration_sync_attempts_project_created_idx",
    "configuration_sync_attempts_project_organization_fk",
    "daemon_enrollment_tokens_issued_by_cli_credential_id_organization_cli_credentials_id_fk",
    "daemon_enrollment_tokens_verifier_unique",
    "daemons_id_organization_unique",
    "daemons_idempotency_key_unique",
    "daemons_machine_id_unique",
    "daemons_machine_organization_fk",
    "daemons_organization_slug_unique",
    "daemons_presence_check",
    "daemons_registered_by_cli_credential_id_organization_cli_credentials_id_fk",
    "daemons_status_check",
    "discord_connections_connected_by_user_id_user_id_fk",
    "discord_connections_guild_id_unique",
    "discord_connections_id_organization_unique",
    "discord_connections_organization_id_organization_id_fk",
    "discord_connections_organization_slug_unique",
    "entitlement_changes_organization_created_idx",
    "entitlement_changes_organization_id_organization_id_fk",
    "entitlement_changes_source_check",
    "execution_authorities_execution_id_agent_executions_id_fk",
    "execution_credential_leases_execution_id_agent_executions_id_fk",
    "execution_credential_leases_execution_idx",
    "github_connections_connected_by_user_id_user_id_fk",
    "github_connections_id_organization_unique",
    "github_connections_installation_id_unique",
    "github_connections_installation_unique",
    "github_connections_organization_id_organization_id_fk",
    "github_connections_organization_idx",
    "github_connections_organization_slug_unique",
    "github_connections_status_check",
    "github_repositories_connection_organization_fk",
    "github_repositories_connection_repository_unique",
    "github_repositories_organization_idx",
    "instance_bootstrap_completion_check",
    "instance_bootstrap_organization_id_organization_id_fk",
    "instance_bootstrap_owner_user_id_user_id_fk",
    "invitation_inviter_id_user_id_fk",
    "invitation_organization_id_organization_id_fk",
    "invitations_organization_status_idx",
    "invitations_pending_organization_email_unique",
    "invitations_role_check",
    "invitations_status_check",
    "linear_connections_connected_by_user_id_user_id_fk",
    "linear_connections_id_organization_unique",
    "linear_connections_linear_organization_id_unique",
    "linear_connections_organization_external_unique",
    "linear_connections_organization_id_organization_id_fk",
    "linear_connections_organization_slug_unique",
    "machines_id_org_id_unique",
    "machines_org_id_idx",
    "machines_status_idx",
    "member_organization_id_organization_id_fk",
    "member_user_id_user_id_fk",
    "members_organization_id_idx",
    "members_organization_user_unique",
    "members_role_check",
    "members_user_id_idx",
    "organization_api_keys_created_by_user_id_user_id_fk",
    "organization_api_keys_organization_created_idx",
    "organization_api_keys_organization_id_organization_id_fk",
    "organization_api_keys_prefix_unique",
    "organization_api_keys_scopes_check",
    "organization_billing_customers_organization_id_organization_id_fk",
    "organization_cli_credentials_created_by_user_id_user_id_fk",
    "organization_cli_credentials_organization_created_idx",
    "organization_cli_credentials_organization_id_organization_id_fk",
    "organization_cli_credentials_prefix_unique",
    "organization_connection_attempts_expiry_idx",
    "organization_connection_attempts_organization_id_organization_id_fk",
    "organization_connection_attempts_phase_check",
    "organization_connection_attempts_provider_check",
    "organization_connection_attempts_session_id_session_id_fk",
    "organization_connection_attempts_shape_check",
    "organization_connection_attempts_state_verifier_unique",
    "organization_connection_attempts_user_id_user_id_fk",
    "organization_entitlements_organization_id_organization_id_fk",
    "organization_slug_unique",
    "organization_trigger_revisions_created_by_user_id_user_id_fk",
    "organization_trigger_revisions_id_trigger_organization_unique",
    "organization_trigger_revisions_source_kind_check",
    "organization_trigger_revisions_trigger_created_idx",
    "organization_trigger_revisions_trigger_organization_fk",
    "organization_trigger_revisions_trigger_version_unique",
    "organization_trigger_routes_resource_idx",
    "organization_trigger_routes_revision_trigger_organization_fk",
    "organization_trigger_routes_shape_unique",
    "organization_trigger_routes_trigger_organization_fk",
    "organization_triggers_active_revision_id_organization_trigger_revisions_id_fk",
    "organization_triggers_format_check",
    "organization_triggers_id_organization_unique",
    "organization_triggers_organization_id_organization_id_fk",
    "organization_triggers_organization_name_unique",
    "organization_triggers_organization_updated_idx",
    "organization_triggers_runtime_project_id_projects_id_fk",
    "organization_usage_organization_id_organization_id_fk",
    "organization_usage_organization_meter_idx",
    "organization_usage_used_non_negative",
    "project_configuration_revisions_created_by_user_id_user_id_fk",
    "project_configuration_revisions_id_project_organization_unique",
    "project_configuration_revisions_project_created_idx",
    "project_configuration_revisions_project_organization_fk",
    "project_configuration_revisions_project_version_unique",
    "project_configuration_revisions_source_kind_check",
    "project_configuration_sources_authority_shape_check",
    "project_configuration_sources_github_connection_organization_fk",
    "project_configuration_sources_project_organization_fk",
    "project_configuration_sources_selected_by_user_id_user_id_fk",
    "project_trigger_migrations_organization_id_organization_id_fk",
    "project_trigger_migrations_organization_idx",
    "project_trigger_migrations_project_id_projects_id_fk",
    "project_trigger_migrations_revision_project_organization_fk",
    "project_trigger_routes_project_organization_fk",
    "project_trigger_routes_provider_check",
    "project_trigger_routes_resource_idx",
    "project_trigger_routes_revision_project_organization_fk",
    "project_trigger_routes_shape_unique",
    "projects_active_configuration_revision_id_project_configuration_revisions_id_fk",
    "projects_archive_shape_check",
    "projects_created_by_user_id_user_id_fk",
    "projects_id_organization_unique",
    "projects_organization_id_organization_id_fk",
    "projects_organization_slug_unique",
    "projects_organization_status_idx",
    "projects_status_check",
    "provider_event_receipts_id_organization_unique",
    "provider_event_receipts_organization_delivery_unique",
    "provider_event_receipts_organization_id_organization_id_fk",
    "provider_event_receipts_organization_received_idx",
    "provider_event_receipts_provider_check",
    "provider_event_receipts_resource_idx",
    "provider_event_receipts_signature_unique",
    "runtime_configuration_singleton_check",
    "runtime_provider_activation_provider_check",
    "runtime_provider_activation_version_check",
    "runtime_provider_configuration_provider_check",
    "runtime_provider_configuration_updated_by_user_id_user_id_fk",
    "runtime_provider_configuration_version_check",
    "session_token_unique",
    "session_user_id_user_id_fk",
    "sessions_active_organization_id_idx",
    "slack_connections_connected_by_user_id_user_id_fk",
    "slack_connections_id_organization_unique",
    "slack_connections_organization_id_organization_id_fk",
    "slack_connections_organization_slug_unique",
    "slack_connections_team_id_unique",
    "trigger_runs_deadline_kind_check",
    "trigger_runs_deadline_shape_check",
    "trigger_runs_organization_fk",
    "trigger_runs_outcome_check",
    "trigger_runs_project_created_idx",
    "trigger_runs_project_organization_fk",
    "trigger_runs_receipt_organization_fk",
    "trigger_runs_receipt_project_configured_unique",
    "trigger_runs_revision_project_organization_fk",
    "trigger_runs_status_check",
    "trigger_runs_status_deadline_idx",
    "trigger_runs_terminal_notification_idx",
    "trigger_schedules_active_run_id_trigger_runs_id_fk",
    "trigger_schedules_due_idx",
    "trigger_schedules_trigger_id_organization_triggers_id_fk",
    "user_email_unique",
    "workflow_step_runs_agent_execution_unique",
    "workflow_step_runs_deadline_kind_check",
    "workflow_step_runs_status_check",
    "workflow_step_runs_trigger_ordinal_unique",
    "workflow_step_runs_trigger_run_fk",
    "workflow_step_runs_trigger_status_idx",
    "workflow_step_runs_trigger_step_unique",
    "workflow_wakeups_available_lease_idx",
    "workflow_wakeups_trigger_run_fk",
];

pub const BASELINE_JOURNAL: &[(u32, &str, i64)] = &[
    (0, "0000_phase_0_spine", 1784319580564),
    (1, "0001_charming_sabretooth", 1784324942281),
    (2, "0002_clean_charles_xavier", 1784338184340),
    (3, "0003_great_zuras", 1784390145792),
    (4, "0004_gigantic_harpoon", 1784420385159),
    (5, "0005_keen_microbe", 1784890014031),
    (6, "0006_overconfident_spacker_dave", 1784902807466),
    (7, "0007_greedy_zuras", 1785699882523),
    (8, "0008_stiff_killer_shrike", 1785703450568),
    (9, "0009_yellow_william_stryker", 1785710041633),
    (10, "0010_classy_strong_guy", 1785713412794),
    (11, "0011_dazzling_robin_chapel", 1785776361717),
    (12, "0012_organization_resource_cutover", 1785835676373),
    (13, "0013_worried_sentinel", 1785859079510),
    (14, "0014_wealthy_joseph", 1785863577166),
    (15, "0015_certain_gateway", 1785924109846),
    (16, "0016_windy_puma", 1785927979208),
    (17, "0017_clever_sasquatch", 1785930920062),
    (18, "0018_silky_cannonball", 1786007639877),
    (19, "0019_cuddly_sunspot", 1786008980236),
    (20, "0020_lean_king_bedlam", 1786015214454),
    (21, "0021_unknown_retro_girl", 1786031867703),
    (22, "0022_burly_whiplash", 1786046610670),
    (23, "0023_robust_vapor", 1786046769096),
    (24, "0024_flippant_young_avengers", 1786057714090),
    (25, "0025_heavy_sumo", 1786059695125),
    (26, "0026_yummy_lord_tyger", 1786062900129),
    (27, "0027_familiar_skin", 1786071206093),
    (28, "0028_outstanding_famine", 1786074362910),
    (29, "0029_lean_miss_america", 1786084312502),
    (30, "0030_chemical_darwin", 1786093287524),
    (31, "0031_old_jackal", 1786104429874),
    (32, "0032_tired_bulldozer", 1786190843844),
    (33, "0033_tense_joseph", 1786372632385),
    (34, "0034_wandering_roxanne_simpson", 1786649830424),
    (35, "0035_smooth_wonder_man", 1786715730006),
    (36, "0036_giant_spyke", 1786732140122),
    (37, "0037_robust_brother_voodoo", 1786744919219),
    (38, "0038_colossal_landau", 1786916556431),
    (39, "0039_neat_flatman", 1787395487866),
    (40, "0040_cultured_punisher", 1787443895467),
    (41, "0041_parched_bucky", 1787949616226),
    (42, "0042_smooth_whizzer", 1788028212397),
    (43, "0043_chubby_sway", 1788028826964),
    (44, "0044_charming_clint_barton", 1788029691057),
    (45, "0045_restore_implicit_trigger_routes", 1788178040836),
    (46, "0046_jittery_absorbing_man", 1788615733084),
    (47, "0047_loving_nuke", 1788961022620),
    (48, "0048_execution_authority", 1788991675061),
];
