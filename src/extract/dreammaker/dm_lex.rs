//! Byte-preserving token recovery; comments and strings never become declarations.

#[derive(Clone, Debug)]
pub(super) struct DmToken {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub quoted: bool,
}

impl DmToken {
    pub fn identifier(&self) -> bool {
        !self.quoted
            && self
                .text
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    }
}

pub(super) struct DmLine {
    pub indent: usize,
    pub end: usize,
    pub tokens: Vec<DmToken>,
}

pub(super) fn tokenize(source: &[u8]) -> (Vec<DmLine>, bool) {
    let mut lines = Vec::with_capacity(source.iter().filter(|&&b| b == b'\n').count() + 1);
    let mut offset = usize::from(source.starts_with(&[0xef, 0xbb, 0xbf])) * 3;
    let mut comment_depth = 0usize;
    let mut multiline_quote = false;
    let mut continued_quote = None;
    let mut complete = true;
    let mut token_count = 0usize;
    while offset < source.len() {
        let line_end = source[offset..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(source.len(), |n| offset + n + 1);
        let mut index = offset;
        let mut indent = 0;
        while index < line_end && matches!(source[index], b' ' | b'\t') {
            indent += if source[index] == b'\t' { 4 } else { 1 };
            index += 1;
        }
        let mut tokens = Vec::with_capacity((line_end - index).min(128) / 4);
        // Oversized lines still update comment/string state but publish no facts.
        let bounded = line_end - offset <= 16 * 1024;
        complete &= bounded;
        while index < line_end {
            let byte = source[index];
            let next = source.get(index + 1).copied();
            if let Some(quote) = continued_quote {
                if byte == b'\\' {
                    index = (index + 2).min(line_end);
                } else {
                    if byte == quote {
                        continued_quote = None;
                    }
                    index += 1;
                }
                continue;
            }
            if comment_depth > 0 {
                if byte == b'/' && next == Some(b'*') {
                    comment_depth += 1;
                    index += 2;
                } else if byte == b'*' && next == Some(b'/') {
                    comment_depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
                continue;
            }
            if multiline_quote {
                if byte == b'\\' {
                    index = (index + 2).min(line_end);
                } else if byte == b'"' && next == Some(b'}') {
                    multiline_quote = false;
                    index += 2;
                } else {
                    index += 1;
                }
                continue;
            }
            if byte.is_ascii_whitespace() {
                index += 1;
                continue;
            }
            if byte == b'/' && next == Some(b'/') {
                break;
            }
            if byte == b'/' && next == Some(b'*') {
                comment_depth = 1;
                index += 2;
                continue;
            }
            if byte == b'{' && next == Some(b'"') {
                multiline_quote = true;
                index += 2;
                continue;
            }
            let start = index;
            let quoted = matches!(byte, b'"' | b'\'');
            if quoted {
                index += 1;
                while index < line_end && source[index] != byte && source[index] != b'\n' {
                    index += if source[index] == b'\\' && index + 1 < line_end {
                        2
                    } else {
                        1
                    };
                }
                if source.get(index) == Some(&byte) {
                    index += 1;
                } else {
                    complete = false;
                    continued_quote = Some(byte);
                }
            } else if byte.is_ascii_alphanumeric() || byte == b'_' {
                index += 1;
                while index < line_end
                    && (source[index].is_ascii_alphanumeric() || source[index] == b'_')
                {
                    index += 1;
                }
            } else {
                index += 1;
            }
            if bounded {
                tokens.push(DmToken {
                    text: String::from_utf8_lossy(&source[start..index]).into_owned(),
                    start,
                    end: index,
                    quoted,
                });
            }
        }
        token_count += tokens.len();
        if token_count > 250_000 {
            complete = false;
            break;
        }
        lines.push(DmLine {
            indent,
            end: line_end,
            tokens,
        });
        offset = line_end;
    }
    complete &= comment_depth == 0 && !multiline_quote && continued_quote.is_none();
    (lines, complete)
}
