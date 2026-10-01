//! The generated public `OpenAPI` document (`src/public-api/openapi.ts`).
//!
//! The Hub builds the document with `@asteasolutions/zod-to-openapi` from its zod schemas. Here the
//! same document is built from the manifest and explicit component definitions, with the same key
//! order and the same component registration order.

use super::json::Json;
use super::manifest::{MANIFEST, OperationDefinition};
use super::operations::scope_name;

fn string_schema(format: Option<&str>, min: Option<u64>, max: Option<u64>) -> Json {
    let mut fields = vec![("type".to_owned(), Json::string("string"))];
    if let Some(format) = format {
        fields.push(("format".to_owned(), Json::string(format)));
    }
    if let Some(min) = min {
        fields.push(("minLength".to_owned(), number(min)));
    }
    if let Some(max) = max {
        fields.push(("maxLength".to_owned(), number(max)));
    }
    Json::Object(fields)
}

#[allow(clippy::cast_precision_loss)]
fn number(value: u64) -> Json {
    Json::Number(value as f64)
}

fn plain() -> Json {
    string_schema(None, None, None)
}

fn uuid() -> Json {
    string_schema(Some("uuid"), None, None)
}

fn positive_integer() -> Json {
    Json::object([
        ("type", Json::string("integer")),
        ("exclusiveMinimum", number(0)),
    ])
}

fn only_true() -> Json {
    Json::object([
        ("type", Json::string("boolean")),
        ("enum", Json::Array(vec![Json::Bool(true)])),
    ])
}

fn string_enum(values: &[&str]) -> Json {
    Json::object([
        ("type", Json::string("string")),
        (
            "enum",
            Json::Array(values.iter().map(|value| Json::string(value)).collect()),
        ),
    ])
}

fn array_of(items: Json, min: Option<u64>, max: Option<u64>) -> Json {
    let mut fields = vec![
        ("type".to_owned(), Json::string("array")),
        ("items".to_owned(), items),
    ];
    if let Some(min) = min {
        fields.push(("minItems".to_owned(), number(min)));
    }
    if let Some(max) = max {
        fields.push(("maxItems".to_owned(), number(max)));
    }
    Json::Object(fields)
}

fn reference(name: &str) -> Json {
    Json::object([("$ref", Json::String(format!("#/components/schemas/{name}")))])
}

/// A strict object: `(name, schema, required)` per property.
fn strict_object(properties: Vec<(&str, Json, bool)>) -> Json {
    let required: Vec<Json> = properties
        .iter()
        .filter(|(_, _, required)| *required)
        .map(|(name, _, _)| Json::string(name))
        .collect();
    let mut fields = vec![
        ("type".to_owned(), Json::string("object")),
        (
            "properties".to_owned(),
            Json::Object(
                properties
                    .into_iter()
                    .map(|(name, schema, _)| (name.to_owned(), schema))
                    .collect(),
            ),
        ),
    ];
    if !required.is_empty() {
        fields.push(("required".to_owned(), Json::Array(required)));
    }
    fields.push(("additionalProperties".to_owned(), Json::Bool(false)));
    Json::Object(fields)
}

fn annotated(schema: Json, description: Option<&str>, example: Option<Json>) -> Json {
    let Json::Object(mut fields) = schema else {
        return schema;
    };
    if let Some(description) = description {
        fields.push(("description".to_owned(), Json::string(description)));
    }
    if let Some(example) = example {
        fields.push(("example".to_owned(), example));
    }
    Json::Object(fields)
}

fn named_strings(pairs: &[(&str, &str)]) -> Json {
    Json::Object(
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), Json::string(value)))
            .collect(),
    )
}

fn github_items() -> Json {
    array_of(
        strict_object(vec![
            ("slug", plain(), true),
            ("accountLogin", plain(), true),
            ("accountType", plain(), true),
            ("repositories", array_of(plain(), None, None), true),
        ]),
        None,
        None,
    )
}

fn pair_items(first: &str, second: &str) -> Json {
    array_of(
        strict_object(vec![(first, plain(), true), (second, plain(), true)]),
        None,
        None,
    )
}

fn installation_example() -> Json {
    let hub = [
        "name: payments",
        "environments:",
        "  runner:",
        "    kind: daemon",
        "    daemon: build-server",
        "    cwd: /workspace",
        "agents:",
        "  default:",
        "    provider: test",
    ]
    .join("\n");
    let workflow = [
        "name: deploy",
        "on: manual.run",
        "max_runtime: 1h",
        "steps:",
        "  - id: deploy",
        "    environment: runner",
        "    max_runtime: 30m",
        "    idle_timeout: 5m",
        "    agent: default",
        "    prompt:",
        "      - include: partials/safety.md",
    ]
    .join("\n");
    Json::object([
        ("projectSlug", Json::string("payments")),
        (
            "files",
            Json::Array(vec![
                named_file(".paseo/hub.yml", &hub),
                named_file(".paseo/workflows/deploy.yml", &workflow),
                named_file(
                    ".paseo/workflows/partials/safety.md",
                    "Follow the safety checklist.",
                ),
            ]),
        ),
    ])
}

fn named_file(path: &str, content: &str) -> Json {
    named_strings(&[("path", path), ("content", content)])
}

/// The component schemas a component refers to, in the order the generator meets them.
fn dependencies(name: &str) -> &'static [&'static str] {
    match name {
        "Problem" => &["FieldIssue"],
        "TriggerList" => &["TriggerExport"],
        "ProjectList" => &["Project"],
        "InstallConfigurationRequest" => &["ConfigurationFile"],
        _ => &[],
    }
}

#[allow(clippy::too_many_lines)]
fn component(name: &str) -> Json {
    match name {
        "FieldIssue" => annotated(
            strict_object(vec![
                (
                    "path",
                    array_of(
                        Json::object([(
                            "anyOf",
                            Json::Array(vec![
                                plain(),
                                Json::object([("type", Json::string("integer"))]),
                            ]),
                        )]),
                        None,
                        None,
                    ),
                    true,
                ),
                ("message", plain(), true),
            ]),
            None,
            Some(Json::object([
                ("path", Json::Array(vec![Json::string("projectSlug")])),
                ("message", Json::string("Required")),
            ])),
        ),
        "TriggerYamlRequest" => annotated(
            strict_object(vec![(
                "yaml",
                string_schema(None, Some(1), Some(1_000_000)),
                true,
            )]),
            Some("One self-contained Paseo trigger YAML document."),
            None,
        ),
        "ValidatedTrigger" => {
            strict_object(vec![("name", plain(), true), ("valid", only_true(), true)])
        }
        "InstalledTrigger" => strict_object(vec![
            ("triggerId", uuid(), true),
            ("name", plain(), true),
            ("revisionId", uuid(), true),
            ("version", positive_integer(), true),
            ("active", only_true(), true),
        ]),
        "StartCliAuthorizationRequest" => strict_object(vec![]),
        "CliAuthorization" => strict_object(vec![
            ("deviceCode", plain(), true),
            ("userCode", plain(), true),
            (
                "verificationUri",
                string_schema(Some("uri"), None, None),
                true,
            ),
            (
                "verificationUriComplete",
                string_schema(Some("uri"), None, None),
                true,
            ),
            (
                "expiresAt",
                string_schema(Some("date-time"), None, None),
                true,
            ),
            ("interval", positive_integer(), true),
        ]),
        "PollCliAuthorizationRequest" => strict_object(vec![(
            "deviceCode",
            string_schema(None, Some(32), Some(200)),
            true,
        )]),
        "CliAuthorizationPoll" => Json::object([(
            "oneOf",
            Json::Array(vec![
                strict_object(vec![
                    ("status", string_enum(&["authorized"]), true),
                    ("interval", positive_integer(), true),
                    ("credential", string_schema(None, Some(1), None), true),
                    ("organizationId", string_schema(None, Some(1), None), true),
                ]),
                strict_object(vec![
                    (
                        "status",
                        string_enum(&["pending", "slow_down", "denied", "expired", "disclosed"]),
                        true,
                    ),
                    ("interval", positive_integer(), true),
                ]),
            ]),
        )]),
        "Problem" => annotated(
            strict_object(vec![
                ("type", string_schema(Some("uri"), None, None), true),
                ("title", plain(), true),
                (
                    "status",
                    Json::object([
                        ("type", Json::string("integer")),
                        ("minimum", number(400)),
                        ("maximum", number(599)),
                    ]),
                    true,
                ),
                ("detail", plain(), true),
                ("code", plain(), true),
                ("requestId", plain(), true),
                (
                    "issues",
                    array_of(reference("FieldIssue"), None, None),
                    false,
                ),
            ]),
            None,
            Some(Json::object([
                (
                    "type",
                    Json::string("https://paseo.sh/problems/invalid-request"),
                ),
                ("title", Json::string("Invalid request")),
                ("status", number(400)),
                (
                    "detail",
                    Json::string("The request body contains invalid fields."),
                ),
                ("code", Json::string("invalid_request")),
                (
                    "requestId",
                    Json::string("5e967c44-fc22-4f6d-8fc5-1bbff33121af"),
                ),
                (
                    "issues",
                    Json::Array(vec![Json::object([
                        ("path", Json::Array(vec![Json::string("projectSlug")])),
                        ("message", Json::string("Required")),
                    ])]),
                ),
            ])),
        ),
        "ConfigurationFile" => annotated(
            strict_object(vec![
                ("path", string_schema(None, Some(1), Some(512)), true),
                ("content", string_schema(None, None, Some(1_000_000)), true),
            ]),
            Some("One UTF-8 file in the canonical .paseo Hub bundle."),
            Some(named_file(
                ".paseo/hub.yml",
                "environments: {}\nagents: {}\n",
            )),
        ),
        "InstallConfigurationRequest" => annotated(
            strict_object(vec![
                (
                    "projectSlug",
                    string_schema(None, Some(1), Some(100)),
                    false,
                ),
                (
                    "files",
                    array_of(reference("ConfigurationFile"), Some(1), Some(100)),
                    true,
                ),
            ]),
            Some(
                "Install the complete canonical bundle: .paseo/hub.yml, direct-child .paseo/workflows/*.yml files, and referenced .paseo/workflows/partials/*.md files.",
            ),
            Some(installation_example()),
        ),
        "InstalledConfiguration" => annotated(
            strict_object(vec![
                ("projectSlug", plain(), true),
                ("versionId", uuid(), true),
                ("version", positive_integer(), true),
                ("active", only_true(), true),
            ]),
            None,
            Some(Json::object([
                ("projectSlug", Json::string("payments")),
                (
                    "versionId",
                    Json::string("84af3583-23ff-4fcc-9838-ed3262499be2"),
                ),
                ("version", number(4)),
                ("active", Json::Bool(true)),
            ])),
        ),
        "ValidatedConfiguration" => annotated(
            strict_object(vec![
                ("projectSlug", plain(), true),
                ("valid", only_true(), true),
                ("wouldCreateProject", only_true(), false),
            ]),
            None,
            Some(Json::object([
                ("projectSlug", Json::string("payments")),
                ("valid", Json::Bool(true)),
                ("wouldCreateProject", Json::Bool(true)),
            ])),
        ),
        "Project" => strict_object(vec![
            ("id", uuid(), true),
            ("name", plain(), true),
            ("slug", plain(), true),
        ]),
        "ProjectList" => annotated(
            strict_object(vec![(
                "projects",
                array_of(reference("Project"), None, None),
                true,
            )]),
            None,
            Some(Json::object([(
                "projects",
                Json::Array(vec![Json::object([
                    ("id", Json::string("84af3583-23ff-4fcc-9838-ed3262499be2")),
                    ("name", Json::string("Payments")),
                    ("slug", Json::string("payments")),
                ])]),
            )])),
        ),
        "TriggerExport" => strict_object(vec![
            ("id", uuid(), true),
            ("name", plain(), true),
            (
                "enabled",
                Json::object([("type", Json::string("boolean"))]),
                true,
            ),
            (
                "format",
                string_enum(&["single_run", "legacy_multistep"]),
                true,
            ),
            ("yaml", plain(), true),
        ]),
        "TriggerList" => strict_object(vec![(
            "triggers",
            array_of(reference("TriggerExport"), None, None),
            true,
        )]),
        "ConfigurationResources" => strict_object(vec![
            (
                "daemons",
                array_of(
                    strict_object(vec![("id", uuid(), true), ("slug", plain(), true)]),
                    None,
                    None,
                ),
                true,
            ),
            ("github", github_items(), true),
            ("discord", pair_items("slug", "guildName"), true),
            ("slack", pair_items("slug", "teamName"), true),
            ("linear", pair_items("slug", "organizationName"), true),
        ]),
        "SetupResources" => strict_object(vec![
            ("github", github_items(), true),
            ("discord", pair_items("guildId", "guildName"), true),
            ("slack", pair_items("teamId", "teamName"), true),
        ]),
        "DispatchManualRunRequest" => annotated(
            strict_object(vec![
                ("projectSlug", string_schema(None, Some(1), Some(100)), true),
                ("expectedVersionId", uuid(), false),
                ("trigger", string_schema(None, Some(1), Some(200)), true),
                ("actor", string_schema(None, Some(1), Some(200)), true),
                ("deliveryKey", string_schema(None, Some(1), Some(200)), true),
                ("input", Json::Object(Vec::new()), false),
            ]),
            None,
            Some(Json::object([
                ("projectSlug", Json::string("payments")),
                ("trigger", Json::string("deploy")),
                ("actor", Json::string("automation")),
                ("deliveryKey", Json::string("deploy-2026-08-06")),
                (
                    "input",
                    Json::object([("environment", Json::string("production"))]),
                ),
            ])),
        ),
        "DispatchedManualRun" => annotated(
            strict_object(vec![
                ("deliveryKey", plain(), true),
                ("providerEventReceiptId", uuid(), true),
                ("triggerRunId", uuid(), true),
                ("configuredTriggerName", plain(), true),
                (
                    "workflowStatus",
                    string_enum(&["running", "succeeded", "failed", "timed_out"]),
                    true,
                ),
            ]),
            None,
            Some(Json::object([
                ("deliveryKey", Json::string("deploy-2026-08-06")),
                (
                    "providerEventReceiptId",
                    Json::string("845e9d26-7977-45e1-bc69-d80a7b55a9cc"),
                ),
                (
                    "triggerRunId",
                    Json::string("f83dc934-02a0-4849-8de7-699110be24ed"),
                ),
                ("configuredTriggerName", Json::string("deploy")),
                ("workflowStatus", Json::string("running")),
            ])),
        ),
        "EnrollmentToken" => annotated(
            strict_object(vec![
                ("token", string_schema(None, Some(32), None), true),
                (
                    "expiresAt",
                    string_schema(Some("date-time"), None, None),
                    true,
                ),
            ]),
            None,
            Some(Json::object([
                ("token", Json::string("one-time-secret-returned-only-once")),
                ("expiresAt", Json::string("2026-08-06T18:10:00.000Z")),
            ])),
        ),
        other => unreachable!("unknown component {other}"),
    }
}

#[derive(Default)]
struct Registry {
    order: Vec<&'static str>,
}

impl Registry {
    fn register(&mut self, name: &'static str) {
        if self.order.contains(&name) {
            return;
        }
        self.order.push(name);
        for dependency in dependencies(name) {
            self.register(dependency);
        }
    }
}

const REQUEST_ID_HEADER: &str = "X-Request-ID";

fn request_id_header() -> Json {
    Json::object([
        (
            "description",
            Json::string("The accepted or generated request identifier."),
        ),
        ("schema", Json::object([("type", Json::string("string"))])),
    ])
}

fn json_content(media_type: &str, component: &str) -> Json {
    Json::object([(
        "content",
        Json::Object(vec![(
            media_type.to_owned(),
            Json::object([("schema", reference(component))]),
        )]),
    )])
}

fn content_pair(media_type: &str, component: &str) -> (String, Json) {
    let Json::Object(mut fields) = json_content(media_type, component) else {
        unreachable!("json_content builds an object");
    };
    fields.remove(0)
}

fn manifest_response(definition: &OperationDefinition, status: u16, description: &str) -> Json {
    let mut headers = vec![(REQUEST_ID_HEADER.to_owned(), request_id_header())];
    let content = if status == definition.success_status {
        content_pair("application/json", definition.success_schema)
    } else {
        if status == 401 {
            headers.push((
                "WWW-Authenticate".to_owned(),
                Json::object([
                    (
                        "description",
                        Json::string("Bearer authentication challenge."),
                    ),
                    (
                        "schema",
                        Json::object([
                            ("type", Json::string("string")),
                            ("example", Json::string("Bearer")),
                        ]),
                    ),
                ]),
            ));
        }
        content_pair("application/problem+json", "Problem")
    };
    Json::Object(vec![
        ("description".to_owned(), Json::string(description)),
        ("headers".to_owned(), Json::Object(headers)),
        content,
    ])
}

fn request_body(component: &str) -> Json {
    Json::object([
        ("required", Json::Bool(true)),
        (
            "content",
            Json::object([(
                "application/json",
                Json::object([("schema", reference(component))]),
            )]),
        ),
    ])
}

fn cli_operation(
    id: &str,
    summary: &str,
    description: &str,
    request: &str,
    responses: Vec<(u16, &str, Option<&str>)>,
) -> Json {
    Json::object([(
        "post",
        Json::object([
            ("operationId", Json::string(id)),
            ("summary", Json::string(summary)),
            ("description", Json::string(description)),
            ("tags", Json::Array(vec![Json::string("CLI login")])),
            ("requestBody", request_body(request)),
            (
                "responses",
                Json::Object(
                    responses
                        .into_iter()
                        .map(|(status, text, schema)| {
                            let mut fields = vec![("description".to_owned(), Json::string(text))];
                            if let Some(schema) = schema {
                                fields.push(content_pair("application/json", schema));
                            }
                            (status.to_string(), Json::Object(fields))
                        })
                        .collect(),
                ),
            ),
        ]),
    )])
}

fn manifest_operation(definition: &OperationDefinition) -> Json {
    let mut fields = vec![
        (
            "operationId".to_owned(),
            Json::string(definition.id.as_str()),
        ),
        ("summary".to_owned(), Json::string(definition.summary)),
        (
            "description".to_owned(),
            Json::string(definition.description),
        ),
        (
            "tags".to_owned(),
            Json::Array(vec![Json::string(definition.tag)]),
        ),
        (
            "security".to_owned(),
            Json::Array(vec![Json::object([(
                "bearerAuth",
                Json::Array(Vec::new()),
            )])]),
        ),
        (
            "x-required-scopes".to_owned(),
            Json::Array(vec![Json::string(scope_name(definition.scope))]),
        ),
    ];
    if let Some(request) = definition.request {
        fields.push(("requestBody".to_owned(), request_body(request.component())));
    }
    fields.push((
        "responses".to_owned(),
        Json::Object(
            definition
                .responses
                .iter()
                .map(|(status, description)| {
                    (
                        status.to_string(),
                        manifest_response(definition, *status, description),
                    )
                })
                .collect(),
        ),
    ));
    Json::Object(vec![(definition.method.to_owned(), Json::Object(fields))])
}

const START_RESPONSES: [(u16, &str, Option<&str>); 3] = [
    (
        201,
        "The CLI login request was created.",
        Some("CliAuthorization"),
    ),
    (429, "Too many active authorization requests.", None),
    (503, "Durable storage is unavailable.", None),
];
const POLL_RESPONSES: [(u16, &str, Option<&str>); 2] = [
    (
        200,
        "The current authorization state.",
        Some("CliAuthorizationPoll"),
    ),
    (503, "Durable storage is unavailable.", None),
];

/// Registers components in the order the generator meets them: for each path in registration
/// order, its responses by ascending status, then its request body.
fn registered_components() -> Registry {
    let mut registry = Registry::default();
    for (_, _, schema) in START_RESPONSES {
        if let Some(schema) = schema {
            registry.register(schema);
        }
    }
    registry.register("StartCliAuthorizationRequest");
    for (_, _, schema) in POLL_RESPONSES {
        if let Some(schema) = schema {
            registry.register(schema);
        }
    }
    registry.register("PollCliAuthorizationRequest");
    for definition in &MANIFEST {
        for (status, _) in definition.responses {
            registry.register(if *status == definition.success_status {
                definition.success_schema
            } else {
                "Problem"
            });
        }
        if let Some(request) = definition.request {
            registry.register(request.component());
        }
    }
    registry
}

fn paths() -> Vec<(String, Json)> {
    let mut paths = vec![
        (
            "/api/v1/cli-authorizations".to_owned(),
            cli_operation(
                "startCliAuthorization",
                "Start CLI login",
                "Starts an anonymous, expiring browser authorization for the Paseo CLI.",
                "StartCliAuthorizationRequest",
                START_RESPONSES.to_vec(),
            ),
        ),
        (
            "/api/v1/cli-authorizations/poll".to_owned(),
            cli_operation(
                "pollCliAuthorization",
                "Poll CLI login",
                "Polls an anonymous CLI login request. An approved credential is disclosed exactly once.",
                "PollCliAuthorizationRequest",
                POLL_RESPONSES.to_vec(),
            ),
        ),
    ];
    for definition in &MANIFEST {
        paths.push((definition.path.to_owned(), manifest_operation(definition)));
    }
    paths
}

/// The document `publicOpenApiDocument` holds.
#[must_use]
pub fn document() -> Json {
    let registry = registered_components();
    let schemas: Vec<(String, Json)> = registry
        .order
        .iter()
        .map(|name| ((*name).to_owned(), component(name)))
        .collect();
    Json::object([
        ("openapi", Json::string("3.1.0")),
        (
            "info",
            Json::object([
                ("title", Json::string("Paseo Hub Public API")),
                ("version", Json::string("1.0.0")),
                (
                    "description",
                    Json::string(
                        "Log in the CLI, list projects, validate and install configuration, dispatch manual runs, and enroll Paseo daemons.",
                    ),
                ),
            ]),
        ),
        (
            "servers",
            Json::Array(vec![named_strings(&[
                ("url", "/"),
                ("description", "This Hub instance"),
            ])]),
        ),
        (
            "tags",
            Json::Array(
                ["CLI login", "Projects", "Configurations", "Runs", "Daemons"]
                    .iter()
                    .map(|name| Json::object([("name", Json::string(name))]))
                    .collect(),
            ),
        ),
        (
            "components",
            Json::object([
                (
                    "securitySchemes",
                    Json::object([(
                        "bearerAuth",
                        named_strings(&[
                            ("type", "http"),
                            ("scheme", "bearer"),
                            ("bearerFormat", "Paseo organization credential"),
                            (
                                "description",
                                "An organization API key or durable CLI login credential. Each operation requires the scope shown on the operation.",
                            ),
                        ]),
                    )]),
                ),
                ("schemas", Json::Object(schemas)),
                ("parameters", Json::Object(Vec::new())),
            ]),
        ),
        ("paths", Json::Object(paths())),
        ("webhooks", Json::Object(Vec::new())),
    ])
}

/// The bytes `publicApi.openapi()` serves: `JSON.stringify` of [`document`].
#[must_use]
pub fn document_text() -> String {
    document().stringify()
}
