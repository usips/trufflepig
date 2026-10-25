//! A Git subprocess that exited unsuccessfully, with its exit code kept typed.
//! Git runs under `LC_ALL=C`; [`GitCommandFailed::found_no_repository`] is the
//! one place its fatal wording is read, because no exit code distinguishes it.
use std::{error::Error, fmt};

/// Exit code Git uses for `fatal:` errors.
const GIT_FATAL_EXIT: i32 = 128;

/// Renders as `Git plumbing failed: STDERR`, with stderr truncated as before.
#[derive(Debug)]
pub struct GitCommandFailed {
    exit_code: Option<i32>,
    stderr: String,
}

impl GitCommandFailed {
    pub(super) fn new(exit_code: Option<i32>, stderr: String) -> Self {
        Self { exit_code, stderr }
    }

    /// The failure in `error`'s chain, if a Git subprocess exited unsuccessfully.
    pub fn from_error(error: &anyhow::Error) -> Option<&Self> {
        error.chain().find_map(|cause| cause.downcast_ref::<Self>())
    }

    /// The process exit code; `None` when a signal ended it.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Whether Git found no repository at or above the working directory.
    pub fn found_no_repository(&self) -> bool {
        self.exit_code == Some(GIT_FATAL_EXIT)
            && self.stderr.starts_with("fatal: not a git repository")
    }
}

impl fmt::Display for GitCommandFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Git plumbing failed: {}", self.stderr)
    }
}

impl Error for GitCommandFailed {}
