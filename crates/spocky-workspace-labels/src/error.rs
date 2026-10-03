//! The errors the label service raises or passes through.

use std::fmt::{Display, Formatter};

use spocky_store::StoreError;

/// `WorkspaceLabelError.code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceLabelErrorCode {
    NameEmpty,
    NotFound,
    NameTaken,
    WorkspaceNotFound,
}

impl WorkspaceLabelErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NameEmpty => "label_name_empty",
            Self::NotFound => "label_not_found",
            Self::NameTaken => "label_name_taken",
            Self::WorkspaceNotFound => "workspace_not_found",
        }
    }
}

/// Everything a label operation rejects with.
#[derive(Debug)]
pub enum LabelError {
    /// `WorkspaceLabelError`: a request the catalog refuses.
    Label {
        code: WorkspaceLabelErrorCode,
        message: &'static str,
    },
    /// `WorkspaceLabelStorageUncertainError`.
    StorageUncertain,
    /// A failed registry step or write, with node's error text.
    Store(StoreError),
    /// A catalog or journal file that could not be read or parsed, with the
    /// text node's `readFile`, `JSON.parse` or zod raised.
    Storage(String),
}

impl LabelError {
    #[must_use]
    pub const fn label(code: WorkspaceLabelErrorCode, message: &'static str) -> Self {
        Self::Label { code, message }
    }

    /// `error.code`, for the errors that carry one.
    #[must_use]
    pub fn code(&self) -> Option<String> {
        match self {
            Self::Label { code, .. } => Some(code.as_str().to_owned()),
            Self::StorageUncertain => Some("workspace_label_storage_uncertain".to_owned()),
            Self::Store(StoreError::Fs(error)) => Some(error.code()),
            Self::Store(_) | Self::Storage(_) => None,
        }
    }
}

impl Display for LabelError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Label { message, .. } => formatter.write_str(message),
            Self::StorageUncertain => formatter.write_str(
                "Workspace label storage outcome is uncertain; restart the daemon before retrying",
            ),
            Self::Store(error) => error.fmt(formatter),
            Self::Storage(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for LabelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StoreError> for LabelError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
