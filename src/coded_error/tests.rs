use super::*;
use anyhow::{Context, anyhow, bail};

#[test]
fn every_code_parses_from_its_own_spelling() {
    for &code in ErrorCode::ALL {
        assert_eq!(ErrorCode::parse(code.as_str()), Some(code));
        assert_eq!(code.to_string(), code.as_str());
    }
    let mut spellings = ErrorCode::ALL
        .iter()
        .map(|code| code.as_str())
        .collect::<Vec<_>>();
    spellings.sort_unstable();
    spellings.dedup();
    assert_eq!(spellings.len(), ErrorCode::ALL.len(), "codes are unique");
}

#[test]
fn raised_codes_render_exactly_code_colon_message() {
    let path = "src/lib.rs";
    let error = anyhow!(coded!(StaleSource, "{path} changed; search again"));
    assert_eq!(
        error.to_string(),
        "stale_source: src/lib.rs changed; search again"
    );
    assert_eq!(format!("{error:#}"), error.to_string());
    assert_eq!(error_code(&error), Some(ErrorCode::StaleSource));
    assert_eq!(
        coded_error(&error).unwrap().message(),
        "src/lib.rs changed; search again"
    );
}

#[test]
fn every_code_renders_as_the_text_it_replaced() {
    for &code in ErrorCode::ALL {
        let cause = || std::io::Error::other("cause");
        let text = format!("{code}: detail {}", 7);
        let raised = anyhow::Error::from(CodedError::new(code, format!("detail {}", 7)));
        assert_eq!(raised.to_string(), anyhow!(text.clone()).to_string());
        let coded = Err::<(), _>(cause())
            .with_coded_context(|| CodedError::new(code, "detail 7"))
            .unwrap_err()
            .context("outer");
        let plain = Err::<(), _>(cause())
            .context(text.clone())
            .unwrap_err()
            .context("outer");
        assert_eq!(format!("{coded:#}"), format!("{plain:#}"), "{code}");
        assert_eq!(format!("{coded}"), format!("{plain}"), "{code}");
        assert_eq!(
            CodedError::from_relayed(format!("daemon: {raised:#}")).map(|relayed| relayed.code()),
            Ok(code),
            "relayed {code}"
        );
    }
}

#[test]
fn coded_context_renders_like_anyhow_context() {
    let io = || std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    let coded = Err::<(), _>(io())
        .with_coded_context(|| coded!(SourceUnavailable, "path missing or unsafe"))
        .unwrap_err();
    let plain = Err::<(), _>(io())
        .context("source_unavailable: path missing or unsafe")
        .unwrap_err();
    for (coded, plain) in [(&coded, &plain)] {
        assert_eq!(coded.to_string(), plain.to_string());
        assert_eq!(format!("{coded:#}"), format!("{plain:#}"));
    }
    assert_eq!(
        format!("{coded:#}"),
        "source_unavailable: path missing or unsafe: gone"
    );
    assert!(ErrorCode::SourceUnavailable.is_code_of(&coded));
    assert!(coded.chain().any(|cause| cause.is::<std::io::Error>()));

    let wrapped = coded.context("show");
    assert_eq!(
        format!("{wrapped:#}"),
        "show: source_unavailable: path missing or unsafe: gone"
    );
    assert_eq!(error_code(&wrapped), Some(ErrorCode::SourceUnavailable));

    let missing = None::<()>
        .with_coded_context(|| coded!(Usage, "show TARGET"))
        .unwrap_err();
    assert_eq!(format!("{missing:#}"), "usage: show TARGET");
}

/// A wrapper that keeps an anyhow error as its source, as `CurrentPathHelp` does.
#[derive(Debug)]
struct HelpfulWrapper(anyhow::Error);

impl fmt::Display for HelpfulWrapper {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}; try again", self.0)
    }
}

impl std::error::Error for HelpfulWrapper {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[test]
fn codes_stay_visible_through_wrapping_errors() {
    let inner = Err::<(), _>(std::io::Error::other("denied"))
        .with_coded_context(|| coded!(SourceUnavailable, "path missing or unsafe"))
        .unwrap_err();
    let wrapped = anyhow::Error::new(HelpfulWrapper(inner));
    assert_eq!(error_code(&wrapped), Some(ErrorCode::SourceUnavailable));
}

#[test]
fn the_outermost_code_wins() {
    let inner = anyhow!(coded!(StaleSource, "changed"));
    let outer = inner.with_coded_context(|| coded!(TimedOut, "query deadline expired"));
    assert_eq!(error_code(&outer), Some(ErrorCode::TimedOut));
    assert_eq!(
        format!("{outer:#}"),
        "timed_out: query deadline expired: stale_source: changed"
    );
}

#[test]
fn bail_and_ensure_keep_the_typed_error() {
    fn bails() -> anyhow::Result<()> {
        bail!(coded!(IndexWarming, "initial reconcile has not published"))
    }
    fn ensures(ready: bool) -> anyhow::Result<()> {
        anyhow::ensure!(ready, coded!(Usage, "show TARGET"));
        Ok(())
    }
    assert!(ErrorCode::IndexWarming.is_code_of(&bails().unwrap_err()));
    let usage = ensures(false).unwrap_err();
    assert_eq!(usage.to_string(), "usage: show TARGET");
    assert!(ErrorCode::Usage.is_code_of(&usage));
}

#[test]
fn uncoded_text_has_no_code() {
    let error = anyhow!("stale_source: plain text is not a code");
    assert_eq!(error_code(&error), None);
}

#[test]
fn details_and_appended_text_keep_the_code() {
    let missing = coded!(SourceUnavailable, "path missing or unsafe")
        .with_detail(CodeDetail::PathMissingOrUnsafe)
        .caused_by(std::io::Error::other("gone"));
    let hinted = missing.appended("; present in parent index");
    assert_eq!(
        hinted.to_string(),
        "source_unavailable: path missing or unsafe; present in parent index"
    );
    let error = anyhow::Error::from(hinted);
    assert_eq!(
        format!("{error:#}"),
        error.to_string(),
        "appended drops the source"
    );
    assert_eq!(error_detail(&error), Some(CodeDetail::PathMissingOrUnsafe));
}

#[test]
fn relayed_text_is_parsed_once_and_kept_verbatim() {
    let relayed = CodedError::from_relayed("daemon: daemon: daemon_busy:  pool=query".into())
        .expect("relayed code");
    assert_eq!(relayed.code(), ErrorCode::DaemonBusy);
    assert!(relayed.is_relay_wrapped());
    assert_eq!(
        relayed.to_string(),
        "daemon: daemon: daemon_busy:  pool=query"
    );
    assert_eq!(relayed.message(), "pool=query");

    let direct = CodedError::from_relayed("timed_out: phase=connecting".into()).unwrap();
    assert!(!direct.is_relay_wrapped());
    let wrapped = direct.relayed_by_daemon();
    assert_eq!(wrapped.to_string(), "daemon: timed_out: phase=connecting");
    assert_eq!(wrapped.message(), "phase=connecting");

    let locked = CodedError::from_relayed("database is locked".into()).unwrap();
    assert_eq!(locked.code(), ErrorCode::DatabaseLocked);

    for text in [
        "load page: timed_out: x",
        "daemon response exceeds 4 MiB",
        "invalid_colour: x",
    ] {
        assert_eq!(
            CodedError::from_relayed(text.into()).unwrap_err(),
            text.to_owned()
        );
    }
}
