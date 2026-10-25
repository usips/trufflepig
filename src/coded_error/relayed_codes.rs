//! Codes named inside text relayed across a process boundary. A relayed
//! reply keeps only its leading code as the typed code; codes spelled later
//! in the text stay discoverable for outcome classification through
//! [`named_codes`], never for control flow, which matches [`super::error_code`].
use super::{CodedError, ErrorCode};
use std::fmt;

/// Relayed failure text that leads with no registered code, kept verbatim.
#[derive(Debug)]
pub struct UncodedRelayedText {
    text: String,
}

impl UncodedRelayedText {
    pub fn new(text: String) -> Self {
        Self { text }
    }
}

impl fmt::Display for UncodedRelayedText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl std::error::Error for UncodedRelayedText {}

/// Types relayed failure text: a [`CodedError`] when a registered code leads
/// it, otherwise [`UncodedRelayedText`].
pub fn relayed_error(text: String) -> anyhow::Error {
    CodedError::from_relayed(text).map_or_else(
        |text| UncodedRelayedText::new(text).into(),
        anyhow::Error::from,
    )
}

/// Every code `error` names, outermost first: each coded layer's code, and
/// each code spelled in relayed text after its leading code. For outcome
/// classification only.
pub fn named_codes(error: &anyhow::Error) -> impl Iterator<Item = ErrorCode> + '_ {
    error.chain().flat_map(|cause| {
        let (leading, relayed) = if let Some(coded) = cause.downcast_ref::<CodedError>() {
            let relayed = coded.is_relayed().then(|| coded.message()).unwrap_or("");
            (Some(coded.code()), relayed)
        } else if let Some(uncoded) = cause.downcast_ref::<UncodedRelayedText>() {
            (None, uncoded.text.as_str())
        } else {
            (None, "")
        };
        leading.into_iter().chain(codes_in_text(relayed))
    })
}

/// Codes spelled as whole snake_case words of `text`. A code without an
/// underscore (`usage`) counts only in `code:` position, so plain English
/// words name nothing.
fn codes_in_text(text: &str) -> impl Iterator<Item = ErrorCode> + '_ {
    let word_byte = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_';
    let bytes = text.as_bytes();
    let mut position = 0;
    std::iter::from_fn(move || {
        while position < bytes.len() {
            while position < bytes.len() && !word_byte(bytes[position]) {
                position += 1;
            }
            let start = position;
            while position < bytes.len() && word_byte(bytes[position]) {
                position += 1;
            }
            let word = &text[start..position];
            let in_code_position = bytes.get(position) == Some(&b':');
            let preceded_by_word = start > 0 && bytes[start - 1].is_ascii_alphanumeric();
            if preceded_by_word || word.is_empty() {
                continue;
            }
            if let Some(code) = ErrorCode::parse(word)
                && (word.contains('_') || in_code_position)
            {
                return Some(code);
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(text: &str) -> Vec<ErrorCode> {
        codes_in_text(text).collect()
    }

    #[test]
    fn whole_code_words_count_and_english_usage_does_not() {
        assert_eq!(
            codes("waited 5 s; index_cache_unavailable (member) usage: x"),
            [ErrorCode::IndexCacheUnavailable, ErrorCode::Usage]
        );
        assert!(codes("see usage below; Resource temporarily unavailable").is_empty());
        assert!(codes("xstale_source and stale_sources and Stale_source").is_empty());
    }

    #[test]
    fn relayed_layers_expose_inner_codes_and_local_layers_do_not() {
        let relayed = relayed_error("daemon: timed_out: wait; index_cache_unavailable".into());
        assert_eq!(
            named_codes(&relayed).collect::<Vec<_>>(),
            [ErrorCode::TimedOut, ErrorCode::IndexCacheUnavailable]
        );
        assert_eq!(
            super::super::error_code(&relayed),
            Some(ErrorCode::TimedOut)
        );
        assert!(!ErrorCode::IndexCacheUnavailable.is_anywhere_in(&relayed));

        let uncoded = relayed_error("load page: budget_too_small: need 900".into());
        assert_eq!(super::super::error_code(&uncoded), None);
        assert_eq!(
            named_codes(&uncoded).collect::<Vec<_>>(),
            [ErrorCode::BudgetTooSmall]
        );

        let local = anyhow::anyhow!(coded!(TimedOut, "mentions index_cache_unavailable"));
        assert_eq!(
            named_codes(&local).collect::<Vec<_>>(),
            [ErrorCode::TimedOut]
        );
    }
}
