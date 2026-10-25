//! Runtime Git version checks for tests that need a newer Git than the
//! product requires. A test whose Git is missing or too old prints why on
//! stderr and returns early, so the build never probes tools on `PATH`.
use std::{process::Command, sync::OnceLock};

/// `(major, minor)` of the `git` on `PATH`, probed once per test process.
fn installed() -> Option<(u32, u32)> {
    static VERSION: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *VERSION.get_or_init(|| {
        let output = Command::new("git").arg("--version").output().ok()?;
        output
            .status
            .success()
            .then(|| parse(&output.stdout))
            .flatten()
    })
}

/// True when the installed Git is at least `major.minor`; otherwise reports
/// the skipped `test` on stderr.
pub(crate) fn at_least(major: u32, minor: u32, test: &str) -> bool {
    let found = installed();
    let supported = found.is_some_and(|version| version >= (major, minor));
    if !supported {
        let found = found.map_or("none".to_owned(), |(major, minor)| {
            format!("{major}.{minor}")
        });
        eprintln!("skipped {test}: requires Git >={major}.{minor} (found {found})");
    }
    supported
}

/// Parses `git --version` stdout into `(major, minor)`.
fn parse(bytes: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut parts = text.split_whitespace().nth(2)?.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[test]
fn git_version_parses_vendor_suffixes_and_rejects_invalid_output() {
    assert_eq!(parse(b"git version 2.55.0.vendor\n"), Some((2, 55)));
    assert_eq!(parse(b"git version two.55"), None);
    assert_eq!(parse(&[0xff]), None);
}
