//! Typed error codes behind the `code: message` text agents parse.
//! Producers raise a [`CodedError`] built by [`coded!`] or attach one with
//! [`CodedContext`]; control flow matches [`error_code`], never rendered text.
//! Text that crosses a process boundary is parsed once by
//! [`CodedError::from_relayed`]. Contract: `docs/diagnostics-contract.md#error-codes`.
mod error_code_inventory;

pub use error_code_inventory::ErrorCode;
pub use relayed_codes::{UncodedRelayedText, named_codes, relayed_error};
use std::fmt;

/// Builds a [`CodedError`] from a code variant and `format!` arguments:
/// `bail!(coded!(StaleSource, "{path} changed"))` renders
/// `stale_source: PATH changed`. Attach one with [`CodedContext`], not
/// anyhow's `context`, so the code stays visible through wrapping errors.
#[macro_export]
macro_rules! coded {
    ($code:ident, $($message:tt)+) => {
        $crate::coded_error::CodedError::new(
            $crate::coded_error::ErrorCode::$code,
            ::std::fmt::format(::std::format_args!($($message)+)),
        )
    };
}

impl ErrorCode {
    /// Reads the code that leads relayed text, after any `daemon:` relay
    /// wrappers, with the detail that follows it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn leading(text: &str) -> Option<(Self, &str)> {
        leading_with_wrappers(text).map(|(code, detail, _)| (code, detail))
    }

    /// Whether `error`'s outermost code is this one.
    pub fn is_code_of(self, error: &anyhow::Error) -> bool {
        error_code(error) == Some(self)
    }

    /// Whether any layer of `error`'s chain carries this code.
    pub fn is_anywhere_in(self, error: &anyhow::Error) -> bool {
        error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<CodedError>())
            .any(|coded| coded.code() == self)
    }

    /// Whether `error` names this code in any layer or in relayed text
    /// ([`named_codes`]); for outcome classification, not control flow.
    pub fn is_named_in(self, error: &anyhow::Error) -> bool {
        named_codes(error).any(|code| code == self)
    }
}

/// The leading code, its detail, and whether `daemon:` wrappers preceded it.
fn leading_with_wrappers(text: &str) -> Option<(ErrorCode, &str, bool)> {
    const RELAY_LABEL: &str = "daemon";
    let mut rest = text;
    let mut wrapped = false;
    loop {
        rest = rest.trim_start();
        if rest.trim_end() == ErrorCode::DatabaseLocked.as_str() {
            return Some((ErrorCode::DatabaseLocked, "", wrapped));
        }
        let (label, detail) = rest.split_once(':')?;
        if label == RELAY_LABEL {
            rest = detail;
            wrapped = true;
            continue;
        }
        return ErrorCode::parse(label).map(|code| (code, detail.trim_start(), wrapped));
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A finer distinction under one code that local matchers need. It never
/// renders and does not survive a process boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeDetail {
    /// `source_unavailable`: a contained path component is missing or unsafe.
    PathMissingOrUnsafe,
    /// `source_unavailable`: the contained path is not a regular file.
    NotRegularFile,
    /// `index_cache_unavailable`: the selected cache itself refused access.
    SelectedCacheRefusal,
    /// `timed_out`: a file-spool wait expired locally, not in the daemon.
    LocalSpoolTimeout,
    /// `timed_out`: the deadline expired while rechecking current source.
    CurrentSourceRecheck,
}

impl CodeDetail {
    /// Whether any coded layer of `error`'s chain carries this detail.
    pub fn is_anywhere_in(self, error: &anyhow::Error) -> bool {
        error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<CodedError>())
            .any(|coded| coded.detail() == Some(self))
    }
}

/// An error with a stable code. It displays exactly its rendered text; its
/// optional source continues the `{error:#}` chain as anyhow context would.
#[derive(Debug)]
pub struct CodedError {
    code: ErrorCode,
    text: String,
    message_start: usize,
    relay_wrapped: bool,
    /// Parsed from text that crossed a process boundary.
    relayed: bool,
    detail: Option<CodeDetail>,
    source: Option<anyhow::Error>,
}

impl CodedError {
    /// Renders `code: message`.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let message = message.into();
        let prefix = code.as_str().len() + 2;
        let mut text = String::with_capacity(prefix + message.len());
        text.push_str(code.as_str());
        text.push_str(": ");
        text.push_str(&message);
        Self {
            code,
            text,
            message_start: prefix,
            relay_wrapped: false,
            relayed: false,
            detail: None,
            source: None,
        }
    }

    pub fn with_detail(self, detail: CodeDetail) -> Self {
        Self {
            detail: Some(detail),
            ..self
        }
    }

    /// Makes `source` the next error in the chain, as anyhow context does.
    pub fn caused_by(self, source: impl Into<anyhow::Error>) -> Self {
        Self {
            source: Some(source.into()),
            ..self
        }
    }

    /// Keeps relayed `text` verbatim when [`ErrorCode::leading`] finds its code;
    /// codes later in the text stay discoverable through [`named_codes`].
    pub fn from_relayed(text: String) -> Result<Self, String> {
        let Some((code, detail, relay_wrapped)) = leading_with_wrappers(&text) else {
            return Err(text);
        };
        let message_start = text.len() - detail.len();
        Ok(Self {
            code,
            text,
            message_start,
            relay_wrapped,
            relayed: true,
            detail: None,
            source: None,
        })
    }

    /// Prefixes one `daemon: ` relay wrapper, keeping the code.
    pub fn relayed_by_daemon(self) -> Self {
        const RELAY_PREFIX: &str = "daemon: ";
        Self {
            text: format!("{RELAY_PREFIX}{}", self.text),
            message_start: self.message_start + RELAY_PREFIX.len(),
            relay_wrapped: true,
            ..self
        }
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// Whether `daemon:` relay wrappers precede the code.
    pub fn is_relay_wrapped(&self) -> bool {
        self.relay_wrapped
    }

    /// Whether this error was parsed from relayed text.
    pub fn is_relayed(&self) -> bool {
        self.relayed
    }

    pub fn detail(&self) -> Option<CodeDetail> {
        self.detail
    }

    /// A sourceless copy whose text gains `suffix`, keeping the code and detail.
    pub fn appended(&self, suffix: &str) -> Self {
        Self {
            code: self.code,
            text: format!("{}{suffix}", self.text),
            message_start: self.message_start,
            relay_wrapped: self.relay_wrapped,
            relayed: self.relayed,
            detail: self.detail,
            source: None,
        }
    }

    /// The text after the leading code.
    pub fn message(&self) -> &str {
        &self.text[self.message_start..]
    }
}

impl fmt::Display for CodedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl std::error::Error for CodedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// Attaches a [`CodedError`] to a failure, like anyhow's `Context`, keeping
/// the failure as its source. A missing `Option` value has no source.
pub trait CodedContext {
    type Output;
    fn with_coded_context(self, coded: impl FnOnce() -> CodedError) -> Self::Output;
}

impl<T, E: Into<anyhow::Error>> CodedContext for Result<T, E> {
    type Output = anyhow::Result<T>;

    fn with_coded_context(self, coded: impl FnOnce() -> CodedError) -> Self::Output {
        self.map_err(|source| coded().caused_by(source).into())
    }
}

impl<T> CodedContext for Option<T> {
    type Output = anyhow::Result<T>;

    fn with_coded_context(self, coded: impl FnOnce() -> CodedError) -> Self::Output {
        self.ok_or_else(|| coded().into())
    }
}

impl CodedContext for anyhow::Error {
    type Output = anyhow::Error;

    fn with_coded_context(self, coded: impl FnOnce() -> CodedError) -> Self::Output {
        coded().caused_by(self).into()
    }
}

/// The outermost [`CodedError`] in `error`'s chain.
pub fn coded_error(error: &anyhow::Error) -> Option<&CodedError> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<CodedError>())
}

/// The outermost code in `error`; `None` for uncoded failures.
pub fn error_code(error: &anyhow::Error) -> Option<ErrorCode> {
    coded_error(error).map(CodedError::code)
}

/// The outermost code's [`CodeDetail`], if it carries one.
pub fn error_detail(error: &anyhow::Error) -> Option<CodeDetail> {
    coded_error(error).and_then(CodedError::detail)
}

// Declared after `coded!` so its tests can raise coded errors.
mod relayed_codes;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod text_match_guard;
