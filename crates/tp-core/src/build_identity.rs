//! The running binary's version and source build hash. The binary installs
//! it at startup, so library code reads the hash at runtime instead of
//! compiling it in and rebuilding on every workspace edit.
use std::sync::OnceLock;

/// The package version and the hash of every workspace source (`build.rs`).
/// Semantic worker handshakes compare it; feedback metadata records it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildIdentity {
    version: &'static str,
    build_id: &'static str,
}

/// Set once per process by the binary entry point. It holds only the
/// binary's compile-time constants, never data computed at runtime.
static INSTALLED: OnceLock<BuildIdentity> = OnceLock::new();

impl BuildIdentity {
    /// What code that never passed through the binary entry point, such as
    /// a library's unit tests, observes.
    pub const UNINSTALLED: Self = Self::new(env!("CARGO_PKG_VERSION"), "uninstalled");

    pub const fn new(version: &'static str, build_id: &'static str) -> Self {
        Self { version, build_id }
    }

    /// Installs this identity for the process; the first installation wins.
    pub fn install(self) {
        let _ = INSTALLED.set(self);
    }

    /// The installed identity, if the binary entry point ran.
    pub fn installed() -> Option<Self> {
        INSTALLED.get().copied()
    }

    /// The installed identity, or [`Self::UNINSTALLED`].
    pub fn current() -> Self {
        Self::installed().unwrap_or(Self::UNINSTALLED)
    }

    pub fn version(self) -> &'static str {
        self.version
    }

    pub fn build_id(self) -> &'static str {
        self.build_id
    }
}
