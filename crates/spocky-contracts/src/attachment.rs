//! Agent prompt attachments: `AgentAttachmentSchema`, the lenient
//! `AgentAttachmentsSchema`, and `ImageAttachmentSchema`.

use serde::de::Deserializer;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use crate::field::{Nullable, optional};
use crate::literal::string_literal;
use crate::number::{NonNegativeInt, PositiveInt};

string_literal!(GitHubPrMimeType = "application/github-pr");
string_literal!(ForgeChangeRequestMimeType = "application/paseo-forge-change-request");
string_literal!(GitHubIssueMimeType = "application/github-issue");
string_literal!(ForgeIssueMimeType = "application/paseo-forge-issue");
string_literal!(TextMimeType = "text/plain");
string_literal!(ReviewMimeType = "application/paseo-review");

fn default_forge() -> String {
    "github".to_owned()
}

/// `GitHubPrAttachmentSchema` fields after `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubPrAttachment {
    #[serde(rename = "mimeType")]
    pub mime_type: GitHubPrMimeType,
    pub number: PositiveInt,
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub body: Option<Nullable<String>>,
    #[serde(
        rename = "baseRefName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub base_ref_name: Option<Nullable<String>>,
    #[serde(
        rename = "headRefName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub head_ref_name: Option<Nullable<String>>,
}

/// `ForgeChangeRequestAttachmentSchema` fields after `type`; `forge`
/// defaults to `"github"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForgeChangeRequestAttachment {
    #[serde(rename = "mimeType")]
    pub mime_type: ForgeChangeRequestMimeType,
    #[serde(default = "default_forge")]
    pub forge: String,
    pub number: PositiveInt,
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub body: Option<Nullable<String>>,
    #[serde(
        rename = "projectPath",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub project_path: Option<String>,
    #[serde(
        rename = "baseRefName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub base_ref_name: Option<Nullable<String>>,
    #[serde(
        rename = "headRefName",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub head_ref_name: Option<Nullable<String>>,
}

/// `GitHubIssueAttachmentSchema` fields after `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubIssueAttachment {
    #[serde(rename = "mimeType")]
    pub mime_type: GitHubIssueMimeType,
    pub number: PositiveInt,
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub body: Option<Nullable<String>>,
}

/// `ForgeIssueAttachmentSchema` fields after `type`; `forge` defaults to
/// `"github"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForgeIssueAttachment {
    #[serde(rename = "mimeType")]
    pub mime_type: ForgeIssueMimeType,
    #[serde(default = "default_forge")]
    pub forge: String,
    pub number: PositiveInt,
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional")]
    pub body: Option<Nullable<String>>,
    #[serde(
        rename = "projectPath",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub project_path: Option<String>,
}

/// `ExternalResourceAttachmentMetadataSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalResourceMetadata {
    pub provider: String,
    #[serde(rename = "providerLabel")]
    pub provider_label: String,
    #[serde(rename = "resourceType")]
    pub resource_type: String,
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub url: String,
}

/// `TextAttachmentSchema` fields after `type`. The schema's transform drops
/// `contextKind` unless it is `"chat_history"` and then appends it last.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextAttachment {
    #[serde(rename = "mimeType")]
    pub mime_type: TextMimeType,
    #[serde(skip_serializing_if = "Option::is_none", with = "optional")]
    pub title: Option<Nullable<String>>,
    pub text: String,
    #[serde(
        rename = "externalResource",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub external_resource: Option<ExternalResourceMetadata>,
    /// Present only as `"chat_history"`.
    #[serde(
        rename = "contextKind",
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub context_kind: Option<String>,
}

impl<'de> Deserialize<'de> for TextAttachment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(rename = "mimeType")]
            mime_type: TextMimeType,
            #[serde(rename = "contextKind", default, with = "optional")]
            context_kind: Option<String>,
            #[serde(default, with = "optional")]
            title: Option<Nullable<String>>,
            text: String,
            #[serde(rename = "externalResource", default, with = "optional")]
            external_resource: Option<ExternalResourceMetadata>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            mime_type: raw.mime_type,
            title: raw.title,
            text: raw.text,
            external_resource: raw.external_resource,
            context_kind: raw.context_kind.filter(|kind| kind == "chat_history"),
        })
    }
}

/// `ReviewAttachmentContextLineSchema.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewLineType {
    Add,
    Remove,
    Context,
}

/// `ReviewAttachmentContextLineSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewContextLine {
    #[serde(rename = "oldLineNumber")]
    pub old_line_number: Option<PositiveInt>,
    #[serde(rename = "newLineNumber")]
    pub new_line_number: Option<PositiveInt>,
    #[serde(rename = "type")]
    pub line_type: ReviewLineType,
    pub content: String,
}

/// `ReviewAttachmentCommentSchema.side`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewSide {
    Old,
    New,
}

/// `ReviewAttachmentCommentSchema.context`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewCommentContext {
    #[serde(rename = "hunkHeader")]
    pub hunk_header: String,
    #[serde(rename = "targetLine")]
    pub target_line: ReviewContextLine,
    pub lines: Vec<ReviewContextLine>,
}

/// `ReviewAttachmentCommentSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewComment {
    #[serde(rename = "filePath")]
    pub file_path: String,
    pub side: ReviewSide,
    #[serde(rename = "lineNumber")]
    pub line_number: PositiveInt,
    pub body: String,
    pub context: ReviewCommentContext,
}

/// `ReviewAttachmentSchema.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewMode {
    Uncommitted,
    Base,
}

/// `ReviewAttachmentSchema` fields after `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewAttachment {
    #[serde(rename = "mimeType")]
    pub mime_type: ReviewMimeType,
    pub cwd: String,
    pub mode: ReviewMode,
    #[serde(
        rename = "baseRef",
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional"
    )]
    pub base_ref: Option<Nullable<String>>,
    pub comments: Vec<ReviewComment>,
}

/// `UploadedFileAttachmentSchema` fields after `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadedFileAttachment {
    pub id: String,
    #[serde(rename = "fileName")]
    pub file_name: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub size: NonNegativeInt,
    pub path: String,
}

/// `AgentAttachmentSchema`, discriminated by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentAttachment {
    #[serde(rename = "forge_change_request")]
    ForgeChangeRequest(ForgeChangeRequestAttachment),
    #[serde(rename = "forge_issue")]
    ForgeIssue(ForgeIssueAttachment),
    #[serde(rename = "github_pr")]
    GitHubPr(GitHubPrAttachment),
    #[serde(rename = "github_issue")]
    GitHubIssue(GitHubIssueAttachment),
    #[serde(rename = "text")]
    Text(TextAttachment),
    #[serde(rename = "review")]
    Review(ReviewAttachment),
    #[serde(rename = "uploaded_file")]
    UploadedFile(UploadedFileAttachment),
}

/// `AgentAttachmentsSchema`: any present value becomes an array of the
/// items that parse as [`AgentAttachment`]; a non-array becomes `[]`.
/// Use with [`optional`] so a missing key stays missing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LenientAttachments(pub Vec<AgentAttachment>);

impl Serialize for LenientAttachments {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LenientAttachments {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let items = match Value::deserialize(deserializer)? {
            Value::Array(items) => items
                .into_iter()
                .filter_map(|item| AgentAttachment::deserialize(item).ok())
                .collect(),
            _ => Vec::new(),
        };
        Ok(Self(items))
    }
}

/// `ImageAttachmentSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    /// Base64 image data.
    pub data: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}
