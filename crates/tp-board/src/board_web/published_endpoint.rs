//! Endpoint identity shared between the serve bind and the signal waiter.
use super::web_endpoint;
use anyhow::Result;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

/// Endpoint identity shared by bind and the pre-bind signal waiter. One
/// mutex covers recording the endpoint and publishing its descriptor, so
/// a signal inside bind runs cleanup either before or after both steps.
#[derive(Default)]
pub(crate) struct PublishedEndpoint {
    slot: Mutex<Option<(PathBuf, SocketAddr)>>,
    #[cfg(test)]
    publish_gate: Option<std::sync::Arc<std::sync::Barrier>>,
    #[cfg(test)]
    require_exit_lock: bool,
}

impl PublishedEndpoint {
    /// Records the endpoint and publishes its descriptor as one locked
    /// step; the exit waiter's cleanup cannot interleave between them.
    pub(super) fn publish(
        &self,
        path: PathBuf,
        address: SocketAddr,
        runtime: &Path,
        database: &Path,
    ) -> Result<()> {
        let mut slot = self.lock();
        *slot = Some((path, address));
        #[cfg(test)]
        if let Some(gate) = &self.publish_gate {
            gate.wait();
        }
        web_endpoint::publish(runtime, address, database)
    }

    /// The recorded endpoint, if bind has published one.
    #[cfg(test)]
    pub(crate) fn endpoint(&self) -> Option<(PathBuf, SocketAddr)> {
        self.lock().clone()
    }

    /// Records an endpoint without publishing its descriptor.
    #[cfg(test)]
    pub(super) fn record_for_test(&self, path: PathBuf, address: SocketAddr) {
        *self.lock() = Some((path, address));
    }

    /// Parks `publish` between recording the endpoint and writing the
    /// descriptor, so a test can fire the signal cleanup in that window.
    #[cfg(test)]
    pub(super) fn gated_for_test(publish_gate: std::sync::Arc<std::sync::Barrier>) -> Self {
        Self {
            slot: Mutex::new(None),
            publish_gate: Some(publish_gate),
            require_exit_lock: false,
        }
    }

    #[cfg(test)]
    pub(super) fn checking_exit_lock_for_test() -> Self {
        Self {
            require_exit_lock: true,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(super) fn check_exit_lock_for_test(&self) {
        if self.require_exit_lock {
            assert!(
                matches!(
                    self.slot.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ),
                "exit waiter released the publication mutex before process exit"
            );
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<(PathBuf, SocketAddr)>> {
        self.slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Remove the published descriptor, if any, and retain the publication lock.
/// The exit waiter holds the returned guard through process exit; cleanup
/// callers that continue release it explicitly.
#[must_use = "retain the publication guard through process exit"]
pub(super) fn remove_published_endpoint(
    published: &PublishedEndpoint,
) -> MutexGuard<'_, Option<(PathBuf, SocketAddr)>> {
    let slot = published.lock();
    if let Some((path, address)) = &*slot {
        web_endpoint::remove_if_ours(path, *address);
    }
    slot
}
