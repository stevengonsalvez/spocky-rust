//! Request schemas and success response schemas of the public API (`src/public-api/contracts.ts`).
//!
//! Requests are parsed into typed inputs or the zod issue list. Success bodies are printed in
//! schema key order; a value that breaks its response schema yields `None`, which the Hub reports
//! as an internal error.

use spocky_contracts::js_value::JsObject;

use super::operations::{
    ConfigurationResources, GithubResource, PublicProject, PublicTrigger, SetupResources,
    TriggerFormat,
};
use super::validation::{
    Issue, PathPart, StringRule, array_field, array_length, index, invalid_type, is_uuid, key,
    object_fields, string_field, unrecognized_keys, utf16_len,
};
use super::value::{JsValueExt as _, Json};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TriggerYamlInput {
    pub yaml: String,
}

impl TriggerYamlInput {
    #[must_use]
    pub fn to_json(&self) -> Json {
        Json::object([("yaml", Json::string(&self.yaml))])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleFile {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallConfigurationInput {
    pub project_slug: Option<String>,
    pub files: Vec<BundleFile>,
}

impl InstallConfigurationInput {
    #[must_use]
    pub fn to_json(&self) -> Json {
        let mut fields = Vec::new();
        if let Some(slug) = &self.project_slug {
            fields.push(("projectSlug".to_owned(), Json::string(slug)));
        }
        fields.push((
            "files".to_owned(),
            Json::Array(
                self.files
                    .iter()
                    .map(|file| {
                        Json::object([
                            ("path", Json::string(&file.path)),
                            ("content", Json::string(&file.content)),
                        ])
                    })
                    .collect(),
            ),
        ));
        Json::from_pairs(fields)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DispatchManualRunInput {
    pub project_slug: String,
    pub expected_version_id: Option<String>,
    pub trigger: String,
    pub actor: String,
    pub delivery_key: String,
    pub input: Json,
}

impl DispatchManualRunInput {
    #[must_use]
    pub fn to_json(&self) -> Json {
        let mut fields = vec![("projectSlug".to_owned(), Json::string(&self.project_slug))];
        if let Some(expected) = &self.expected_version_id {
            fields.push(("expectedVersionId".to_owned(), Json::string(expected)));
        }
        fields.push(("trigger".to_owned(), Json::string(&self.trigger)));
        fields.push(("actor".to_owned(), Json::string(&self.actor)));
        fields.push(("deliveryKey".to_owned(), Json::string(&self.delivery_key)));
        fields.push(("input".to_owned(), self.input.clone()));
        Json::from_pairs(fields)
    }
}

fn field<'a>(fields: &'a JsObject, name: &str) -> Option<&'a Json> {
    fields.get(name)
}

const SLUG: StringRule = StringRule {
    min: Some(1),
    max: Some(100),
    trim: true,
    uuid: false,
};
const NAME_200: StringRule = StringRule {
    min: Some(1),
    max: Some(200),
    trim: true,
    uuid: false,
};

fn finish<T>(issues: Vec<Issue>, value: Option<T>) -> Result<T, Vec<Issue>> {
    match value {
        Some(value) if issues.is_empty() => Ok(value),
        _ => Err(issues),
    }
}

/// # Errors
///
/// Returns the schema issues in zod order.
pub fn parse_trigger_yaml(root: &Json) -> Result<TriggerYamlInput, Vec<Issue>> {
    let mut issues = Vec::new();
    let Some(fields) = object_fields(Some(root), &[], &mut issues) else {
        return Err(issues);
    };
    let yaml = string_field(
        field(fields, "yaml"),
        false,
        &key(&[], "yaml"),
        StringRule {
            min: Some(1),
            max: Some(1_000_000),
            trim: false,
            uuid: false,
        },
        &mut issues,
    );
    unrecognized_keys(fields, &["yaml"], &[], &mut issues);
    finish(issues, yaml.map(|yaml| TriggerYamlInput { yaml }))
}

fn parse_file(value: &Json, path: &[PathPart], issues: &mut Vec<Issue>) -> Option<BundleFile> {
    let fields = object_fields(Some(value), path, issues)?;
    let file_path = string_field(
        field(fields, "path"),
        false,
        &key(path, "path"),
        StringRule {
            min: Some(1),
            max: Some(512),
            trim: false,
            uuid: false,
        },
        issues,
    );
    let content = string_field(
        field(fields, "content"),
        false,
        &key(path, "content"),
        StringRule {
            min: None,
            max: Some(1_000_000),
            trim: false,
            uuid: false,
        },
        issues,
    );
    unrecognized_keys(fields, &["path", "content"], path, issues);
    Some(BundleFile {
        path: file_path?,
        content: content?,
    })
}

/// # Errors
///
/// Returns the schema issues in zod order.
pub fn parse_install_configuration(root: &Json) -> Result<InstallConfigurationInput, Vec<Issue>> {
    let mut issues = Vec::new();
    let Some(fields) = object_fields(Some(root), &[], &mut issues) else {
        return Err(issues);
    };
    let project_slug = string_field(
        field(fields, "projectSlug"),
        true,
        &key(&[], "projectSlug"),
        SLUG,
        &mut issues,
    );
    let files_path = key(&[], "files");
    let mut files = None;
    if let Some(items) = array_field(field(fields, "files"), &files_path, 1, 100, &mut issues) {
        let parsed: Vec<Option<BundleFile>> = items
            .iter()
            .enumerate()
            .map(|(position, item)| parse_file(item, &index(&files_path, position), &mut issues))
            .collect();
        array_length(items.len(), &files_path, 1, 100, &mut issues);
        files = parsed.into_iter().collect::<Option<Vec<_>>>();
    }
    unrecognized_keys(fields, &["projectSlug", "files"], &[], &mut issues);
    finish(
        issues,
        files.map(|files| InstallConfigurationInput {
            project_slug,
            files,
        }),
    )
}

/// # Errors
///
/// Returns the schema issues in zod order.
pub fn parse_dispatch_manual_run(root: &Json) -> Result<DispatchManualRunInput, Vec<Issue>> {
    let mut issues = Vec::new();
    let Some(fields) = object_fields(Some(root), &[], &mut issues) else {
        return Err(issues);
    };
    let project_slug = string_field(
        field(fields, "projectSlug"),
        false,
        &key(&[], "projectSlug"),
        SLUG,
        &mut issues,
    );
    let expected_version_id = string_field(
        field(fields, "expectedVersionId"),
        true,
        &key(&[], "expectedVersionId"),
        StringRule {
            min: None,
            max: None,
            trim: false,
            uuid: true,
        },
        &mut issues,
    );
    let trigger = string_field(
        field(fields, "trigger"),
        false,
        &key(&[], "trigger"),
        NAME_200,
        &mut issues,
    );
    let actor = string_field(
        field(fields, "actor"),
        false,
        &key(&[], "actor"),
        NAME_200,
        &mut issues,
    );
    let delivery_key = string_field(
        field(fields, "deliveryKey"),
        false,
        &key(&[], "deliveryKey"),
        NAME_200,
        &mut issues,
    );
    let input = field(fields, "input").cloned();
    if input.is_none() {
        issues.push(Issue::new(
            &key(&[], "input"),
            "Invalid input: expected nonoptional, received undefined".to_owned(),
        ));
    }
    unrecognized_keys(
        fields,
        &[
            "projectSlug",
            "expectedVersionId",
            "trigger",
            "actor",
            "deliveryKey",
            "input",
        ],
        &[],
        &mut issues,
    );
    let present_expected = field(fields, "expectedVersionId").is_some();
    finish(
        issues,
        (|| {
            Some(DispatchManualRunInput {
                project_slug: project_slug?,
                expected_version_id: if present_expected {
                    Some(expected_version_id?)
                } else {
                    None
                },
                trigger: trigger?,
                actor: actor?,
                delivery_key: delivery_key?,
                input: input?,
            })
        })(),
    )
}

/// `z.object({}).strict()`: an object with no keys other than `__proto__`.
#[must_use]
pub fn is_empty_object(root: &Json) -> bool {
    let mut issues = Vec::new();
    let Some(fields) = object_fields(Some(root), &[], &mut issues) else {
        return false;
    };
    unrecognized_keys(fields, &[], &[], &mut issues);
    issues.is_empty()
}

/// `{ deviceCode: string(32..=200) }`, strict.
#[must_use]
pub fn parse_device_code(root: &Json) -> Option<String> {
    let mut issues = Vec::new();
    let fields = object_fields(Some(root), &[], &mut issues)?;
    let code = string_field(
        field(fields, "deviceCode"),
        false,
        &key(&[], "deviceCode"),
        StringRule {
            min: Some(32),
            max: Some(200),
            trim: false,
            uuid: false,
        },
        &mut issues,
    );
    unrecognized_keys(fields, &["deviceCode"], &[], &mut issues);
    issues.is_empty().then_some(code).flatten()
}

fn user_code_rule() -> StringRule {
    StringRule {
        min: Some(1),
        max: Some(40),
        trim: false,
        uuid: false,
    }
}

/// `{ userCode: string(1..=40) }`, strict.
#[must_use]
pub fn parse_user_code(root: &Json) -> Option<String> {
    let mut issues = Vec::new();
    let fields = object_fields(Some(root), &[], &mut issues)?;
    let code = string_field(
        field(fields, "userCode"),
        false,
        &key(&[], "userCode"),
        user_code_rule(),
        &mut issues,
    );
    unrecognized_keys(fields, &["userCode"], &[], &mut issues);
    issues.is_empty().then_some(code).flatten()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionBody {
    pub user_code: String,
    pub approve: bool,
    pub organization_id: String,
}

/// `{ userCode, decision: "approve" | "deny", organizationId: string(1..) }`, strict.
#[must_use]
pub fn parse_decision(root: &Json) -> Option<DecisionBody> {
    let mut issues = Vec::new();
    let fields = object_fields(Some(root), &[], &mut issues)?;
    let user_code = string_field(
        field(fields, "userCode"),
        false,
        &key(&[], "userCode"),
        user_code_rule(),
        &mut issues,
    );
    let decision = match field(fields, "decision") {
        Some(Json::String(text)) if text == "approve" => Some(true),
        Some(Json::String(text)) if text == "deny" => Some(false),
        other => {
            invalid_type(
                &key(&[], "decision"),
                "\"approve\" | \"deny\"",
                other,
                &mut issues,
            );
            None
        }
    };
    let organization_id = string_field(
        field(fields, "organizationId"),
        false,
        &key(&[], "organizationId"),
        StringRule {
            min: Some(1),
            max: None,
            trim: false,
            uuid: false,
        },
        &mut issues,
    );
    unrecognized_keys(
        fields,
        &["userCode", "decision", "organizationId"],
        &[],
        &mut issues,
    );
    if !issues.is_empty() {
        return None;
    }
    Some(DecisionBody {
        user_code: user_code?,
        approve: decision?,
        organization_id: organization_id?,
    })
}

fn uuid(value: &str) -> Option<Json> {
    is_uuid(value).then(|| Json::string(value))
}

fn positive(value: i64) -> Option<Json> {
    (value > 0).then(|| Json::integer(value))
}

fn strings(values: &[String]) -> Json {
    Json::Array(values.iter().map(|value| Json::string(value)).collect())
}

fn github(resources: &[GithubResource]) -> Json {
    Json::Array(
        resources
            .iter()
            .map(|resource| {
                Json::object([
                    ("slug", Json::string(&resource.slug)),
                    ("accountLogin", Json::string(&resource.account_login)),
                    ("accountType", Json::string(&resource.account_type)),
                    ("repositories", strings(&resource.repositories)),
                ])
            })
            .collect(),
    )
}

fn pairs(values: &[(String, String)], first: &str, second: &str) -> Json {
    Json::Array(
        values
            .iter()
            .map(|(left, right)| {
                Json::object([(first, Json::string(left)), (second, Json::string(right))])
            })
            .collect(),
    )
}

#[must_use]
pub fn triggers_body(triggers: &[PublicTrigger]) -> Option<Json> {
    let mut items = Vec::new();
    for trigger in triggers {
        items.push(Json::object([
            ("id", uuid(&trigger.id)?),
            ("name", Json::string(&trigger.name)),
            ("enabled", Json::Bool(trigger.enabled)),
            (
                "format",
                Json::string(match trigger.format {
                    TriggerFormat::SingleRun => "single_run",
                    TriggerFormat::LegacyMultistep => "legacy_multistep",
                }),
            ),
            ("yaml", Json::string(&trigger.yaml)),
        ]));
    }
    Some(Json::object([("triggers", Json::Array(items))]))
}

#[must_use]
pub fn validated_trigger_body(name: &str) -> Json {
    Json::object([("name", Json::string(name)), ("valid", Json::Bool(true))])
}

#[must_use]
pub fn installed_trigger_body(
    trigger_id: &str,
    name: &str,
    revision_id: &str,
    version: i64,
) -> Option<Json> {
    Some(Json::object([
        ("triggerId", uuid(trigger_id)?),
        ("name", Json::string(name)),
        ("revisionId", uuid(revision_id)?),
        ("version", positive(version)?),
        ("active", Json::Bool(true)),
    ]))
}

#[must_use]
pub fn projects_body(projects: &[PublicProject]) -> Option<Json> {
    let mut items = Vec::new();
    for project in projects {
        items.push(Json::object([
            ("id", uuid(&project.id)?),
            ("name", Json::string(&project.name)),
            ("slug", Json::string(&project.slug)),
        ]));
    }
    Some(Json::object([("projects", Json::Array(items))]))
}

#[must_use]
pub fn configuration_resources_body(resources: &ConfigurationResources) -> Option<Json> {
    let mut daemons = Vec::new();
    for (id, slug) in &resources.daemons {
        daemons.push(Json::object([
            ("id", uuid(id)?),
            ("slug", Json::string(slug)),
        ]));
    }
    Some(Json::object([
        ("daemons", Json::Array(daemons)),
        ("github", github(&resources.github)),
        ("discord", pairs(&resources.discord, "slug", "guildName")),
        ("slack", pairs(&resources.slack, "slug", "teamName")),
        (
            "linear",
            pairs(&resources.linear, "slug", "organizationName"),
        ),
    ]))
}

#[must_use]
pub fn setup_resources_body(resources: &SetupResources) -> Json {
    Json::object([
        ("github", github(&resources.github)),
        ("discord", pairs(&resources.discord, "guildId", "guildName")),
        ("slack", pairs(&resources.slack, "teamId", "teamName")),
    ])
}

#[must_use]
pub fn validated_configuration_body(project_slug: &str, would_create_project: bool) -> Json {
    let mut fields = vec![
        ("projectSlug".to_owned(), Json::string(project_slug)),
        ("valid".to_owned(), Json::Bool(true)),
    ];
    if would_create_project {
        fields.push(("wouldCreateProject".to_owned(), Json::Bool(true)));
    }
    Json::from_pairs(fields)
}

#[must_use]
pub fn installed_configuration_body(
    project_slug: &str,
    version_id: &str,
    version: i64,
    active: bool,
) -> Option<Json> {
    Some(Json::object([
        ("projectSlug", Json::string(project_slug)),
        ("versionId", uuid(version_id)?),
        ("version", positive(version)?),
        ("active", active.then_some(Json::Bool(true))?),
    ]))
}

#[must_use]
pub fn dispatched_run_body(
    delivery_key: &str,
    provider_event_receipt_id: &str,
    trigger_run_id: &str,
    configured_trigger_name: &str,
    workflow_status: &str,
) -> Option<Json> {
    Some(Json::object([
        ("deliveryKey", Json::string(delivery_key)),
        ("providerEventReceiptId", uuid(provider_event_receipt_id)?),
        ("triggerRunId", uuid(trigger_run_id)?),
        (
            "configuredTriggerName",
            Json::string(configured_trigger_name),
        ),
        ("workflowStatus", Json::string(workflow_status)),
    ]))
}

#[must_use]
pub fn enrollment_token_body(token: &str, expires_at_ms: i64) -> Option<Json> {
    if utf16_len(token) < 32 {
        return None;
    }
    Some(Json::object([
        ("token", Json::string(token)),
        ("expiresAt", Json::String(iso_string(expires_at_ms)?)),
    ]))
}

/// `Date#toISOString` for epoch milliseconds, or `None` outside the Date range.
#[must_use]
pub fn iso_string(epoch_ms: i64) -> Option<String> {
    if epoch_ms.abs() > 8_640_000_000_000_000 {
        return None;
    }
    let days = epoch_ms.div_euclid(86_400_000);
    let millis_of_day = epoch_ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let year_text = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{}{:06}", if year < 0 { '-' } else { '+' }, year.abs())
    };
    let hour = millis_of_day / 3_600_000;
    let minute = millis_of_day / 60_000 % 60;
    let second = millis_of_day / 1000 % 60;
    let milli = millis_of_day % 1000;
    Some(format!(
        "{year_text}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z"
    ))
}

/// Proleptic Gregorian date of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::iso_string;

    #[test]
    fn iso_strings_match_to_iso_string() {
        assert_eq!(iso_string(0).as_deref(), Some("1970-01-01T00:00:00.000Z"));
        assert_eq!(
            iso_string(1_786_039_800_123).as_deref(),
            Some("2026-08-06T18:10:00.123Z")
        );
        assert_eq!(
            iso_string(-62_167_219_200_000).as_deref(),
            Some("0000-01-01T00:00:00.000Z")
        );
        assert_eq!(
            iso_string(-62_198_755_200_000).as_deref(),
            Some("-000001-01-01T00:00:00.000Z")
        );
        assert_eq!(
            iso_string(8_640_000_000_000_000).as_deref(),
            Some("+275760-09-13T00:00:00.000Z")
        );
        assert_eq!(
            iso_string(-8_640_000_000_000_000).as_deref(),
            Some("-271821-04-20T00:00:00.000Z")
        );
        assert_eq!(iso_string(8_640_000_000_000_001), None);
    }
}
