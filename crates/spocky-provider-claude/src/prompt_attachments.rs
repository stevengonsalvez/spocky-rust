//! `renderPromptAttachmentAsText` from `agent/prompt-attachments.ts`, with
//! the forge labels of `@getpaseo/protocol/forge-manifest`.

use spocky_contracts::js::{js_string, truthy};
use spocky_contracts::js_value::JsValue;
use spocky_session::agent_sdk::AgentError;

/// `(displayName, changeRequestAbbrev, changeRequestNumberPrefix,
/// issueNumberPrefix)` of `getForgeDefinitionOrNeutral(id)`.
fn forge(id: &str) -> (String, &'static str, &'static str, &'static str) {
    match id {
        "github" => ("GitHub".to_owned(), "PR", "#", "#"),
        "gitlab" => ("GitLab".to_owned(), "MR", "!", "#"),
        "gitea" => ("Gitea".to_owned(), "PR", "#", "#"),
        "forgejo" => ("Forgejo".to_owned(), "PR", "#", "#"),
        "codeberg" => ("Codeberg".to_owned(), "PR", "#", "#"),
        other => (other.to_owned(), "PR", "#", "#"),
    }
}

fn field<'a>(attachment: &'a JsValue, key: &str) -> Option<&'a JsValue> {
    attachment.get(key)
}

fn string(attachment: &JsValue, key: &str) -> String {
    js_string(field(attachment, key))
}

fn push_if_truthy(lines: &mut Vec<String>, label: &str, value: Option<&JsValue>) {
    if truthy(value) {
        lines.push(format!("{label}{}", js_string(value)));
    }
}

fn change_request(attachment: &JsValue, forge_id: &str, with_project: bool) -> String {
    let (label, abbrev, prefix, _) = forge(forge_id);
    let mut lines = vec![
        format!(
            "{label} {abbrev} {prefix}{}: {}",
            string(attachment, "number"),
            string(attachment, "title")
        ),
        string(attachment, "url"),
    ];
    if with_project {
        push_if_truthy(&mut lines, "Project: ", field(attachment, "projectPath"));
    }
    push_if_truthy(&mut lines, "Base: ", field(attachment, "baseRefName"));
    push_if_truthy(&mut lines, "Head: ", field(attachment, "headRefName"));
    if truthy(field(attachment, "body")) {
        lines.push(String::new());
        lines.push(string(attachment, "body"));
    }
    lines.join("\n")
}

fn issue(attachment: &JsValue, forge_id: &str, with_project: bool) -> String {
    let (label, _, _, prefix) = forge(forge_id);
    let mut lines = vec![
        format!(
            "{label} Issue {prefix}{}: {}",
            string(attachment, "number"),
            string(attachment, "title")
        ),
        string(attachment, "url"),
    ];
    if with_project {
        push_if_truthy(&mut lines, "Project: ", field(attachment, "projectPath"));
    }
    if truthy(field(attachment, "body")) {
        lines.push(String::new());
        lines.push(string(attachment, "body"));
    }
    lines.join("\n")
}

/// `(lineNumber?.toString() ?? "-").padStart(2)`.
fn pad_line_number(value: Option<&JsValue>) -> String {
    let text = match value {
        None | Some(JsValue::Undefined | JsValue::Null) => "-".to_owned(),
        Some(other) => js_string(Some(other)),
    };
    let length = spocky_contracts::text::js_length(&text);
    if length >= 2 {
        text
    } else {
        format!("{}{text}", " ".repeat(2 - length))
    }
}

fn review(attachment: &JsValue) -> String {
    let mut lines = vec![
        format!("Paseo review attachment ({})", string(attachment, "mode")),
        format!("CWD: {}", string(attachment, "cwd")),
    ];
    push_if_truthy(&mut lines, "Base: ", field(attachment, "baseRef"));
    let comments = field(attachment, "comments")
        .and_then(JsValue::as_array)
        .unwrap_or_default();
    for (index, comment) in comments.iter().enumerate() {
        let context = comment.get("context");
        lines.push(String::new());
        lines.push(format!(
            "Comment {}: {}:{}:{}",
            index + 1,
            js_string(comment.get("filePath")),
            js_string(comment.get("side")),
            js_string(comment.get("lineNumber"))
        ));
        lines.push(js_string(comment.get("body")));
        lines.push(js_string(
            context.and_then(|context| context.get("hunkHeader")),
        ));
        let target = context.and_then(|context| context.get("targetLine"));
        let read = |line: Option<&JsValue>, key: &str| line.and_then(|line| line.get(key)).cloned();
        for line in context
            .and_then(|context| context.get("lines"))
            .and_then(JsValue::as_array)
            .unwrap_or_default()
        {
            let is_target = ["oldLineNumber", "newLineNumber", "type", "content"]
                .iter()
                .all(|key| read(Some(line), key) == read(target, key));
            let marker = match line.get("type").and_then(JsValue::as_str) {
                Some("add") => "+".to_owned(),
                Some("remove") => "-".to_owned(),
                Some("context") => " ".to_owned(),
                _ => "undefined".to_owned(),
            };
            lines.push(format!(
                "{}{} {} {marker}{}",
                if is_target { "> " } else { "  " },
                pad_line_number(line.get("oldLineNumber")),
                pad_line_number(line.get("newLineNumber")),
                js_string(line.get("content"))
            ));
        }
    }
    lines.join("\n")
}

/// `renderPromptAttachmentAsText(attachment)`.
///
/// # Errors
///
/// `unreachable` for an attachment type the baseline does not know.
pub fn render_prompt_attachment_as_text(attachment: &JsValue) -> Result<String, AgentError> {
    let kind = attachment.get("type").and_then(JsValue::as_str);
    Ok(match kind {
        Some("forge_change_request") => {
            change_request(attachment, &string(attachment, "forge"), true)
        }
        Some("github_pr") => change_request(attachment, "github", false),
        Some("forge_issue") => issue(attachment, &string(attachment, "forge"), true),
        Some("github_issue") => issue(attachment, "github", false),
        Some("text") => string(attachment, "text"),
        Some("review") => review(attachment),
        Some("uploaded_file") => [
            format!("Uploaded file: {}", string(attachment, "fileName")),
            format!("Path: {}", string(attachment, "path")),
            format!("MIME: {}", string(attachment, "mimeType")),
            format!("Size: {} bytes", string(attachment, "size")),
        ]
        .join("\n"),
        _ => return Err(AgentError::new("unreachable")),
    })
}
