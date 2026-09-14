//! Conservative retained-payload budgets reserve room for simultaneous pipeline stages.
use anyhow::{Result, ensure};
use serde::Serialize;
use std::io::{self, Write};

pub(super) const MIB: usize = 1024 * 1024;
pub(super) const MAX_STAGING: usize = 64 * MIB;
pub(super) const TREE_BYTES: usize = 6 * MIB;
pub(super) const AUXILIARY_BYTES: usize = 4 * MIB;
pub(super) const CHANGE_BYTES: usize = 8 * MIB;
pub(super) const CACHE_BYTES: usize = 8 * MIB;
pub(super) const ENTRY_BYTES: usize = 16 * MIB;
pub(super) const HUNK_BYTES: usize = 8 * MIB;
pub(super) const PATH_BYTES: usize = 128 * 1024;

const _: () = assert!(
    2 * TREE_BYTES
        + AUXILIARY_BYTES
        + CHANGE_BYTES
        + CACHE_BYTES
        + ENTRY_BYTES
        + HUNK_BYTES
        + super::git::MAX_OUTPUT_BYTES
        <= MAX_STAGING
);

pub(super) struct StagingBudget {
    used: usize,
    limit: usize,
}
impl StagingBudget {
    pub fn new(limit: usize) -> Self {
        Self { used: 0, limit }
    }
    pub fn reserve(&mut self, bytes: usize) -> Result<()> {
        ensure!(
            bytes <= self.limit.saturating_sub(self.used),
            "history_resource_limited: retained staging capacity reached"
        );
        self.used += bytes;
        Ok(())
    }
}

pub(super) fn entry_charge(entry: &crate::results::ResultEntry) -> Result<usize> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter(0);
    serde_json::to_writer(&mut count, entry)?;
    Ok(count.0 + 2 * std::mem::size_of::<crate::results::ResultEntry>())
}

pub(super) fn bounded_json(value: &impl Serialize, limit: usize) -> Result<String> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(io::Error::other(
                    "history_resource_limited: serialized staging capacity reached",
                ));
            }
            if self.bytes.capacity() - self.bytes.len() < bytes.len() {
                let capacity = (self.bytes.len() + bytes.len()).div_ceil(64 * 1024) * (64 * 1024);
                self.bytes
                    .reserve_exact(capacity.min(self.limit) - self.bytes.len());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: Vec::with_capacity(limit.min(4096)),
        limit,
    };
    serde_json::to_writer(&mut buffer, value)?;
    Ok(String::from_utf8(buffer.bytes).expect("JSON UTF-8"))
}
