use crate::diff::FileDiff;

/// Largest source revision Mig will load and expand into review rows.
pub const MAX_REVISION_BYTES: u64 = 16 * 1024 * 1024;

/// One path in the review ribbon, whether diffable or deliberately deferred.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileReview {
    Diff(FileDiff),
    Notice(FileNotice),
}

impl FileReview {
    pub fn path(&self) -> &str {
        match self {
            Self::Diff(diff) => &diff.path,
            Self::Notice(notice) => notice.path(),
        }
    }

    /// Generated ordering applies only after source was safe to inspect.
    pub fn is_generated(&self) -> bool {
        match self {
            Self::Diff(diff) => diff.generated,
            Self::Notice(_) => false,
        }
    }
}

impl From<FileDiff> for FileReview {
    fn from(diff: FileDiff) -> Self {
        Self::Diff(diff)
    }
}

/// A changed path retained in navigation even though its content was not loaded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileNotice {
    TooLarge {
        path: String,
        before_bytes: Option<u64>,
        after_bytes: Option<u64>,
        limit_bytes: u64,
    },
}

impl FileNotice {
    pub fn too_large(
        path: impl Into<String>,
        before_bytes: Option<u64>,
        after_bytes: Option<u64>,
        limit_bytes: u64,
    ) -> Self {
        Self::TooLarge {
            path: path.into(),
            before_bytes,
            after_bytes,
            limit_bytes,
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Self::TooLarge { path, .. } => path,
        }
    }
}
