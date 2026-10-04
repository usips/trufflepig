use super::{BODY_LIMIT, HttpError, HttpMethod, HttpRequest};
use std::collections::BTreeMap;

pub(super) fn check_line_endings(bytes: &[u8]) -> Result<(), HttpError> {
    check_line_endings_from(bytes, 0)
}

/// Incremental variant for the read loop: bytes before `from` passed an
/// earlier check; the caller keeps a trailing CR below `from` for re-check.
pub(super) fn check_line_endings_from(bytes: &[u8], from: usize) -> Result<(), HttpError> {
    for (index, byte) in bytes.iter().copied().enumerate().skip(from) {
        if (byte == b'\n' && index.checked_sub(1).is_none_or(|prev| bytes[prev] != b'\r'))
            || (byte == b'\r' && bytes.get(index + 1).is_some_and(|next| *next != b'\n'))
        {
            return Err(HttpError::new(400, "CRLF line endings required"));
        }
    }
    Ok(())
}

pub(super) fn parse_headers(bytes: &[u8]) -> Result<HttpRequest, HttpError> {
    check_line_endings(bytes)?;
    if bytes.iter().any(|byte| !byte.is_ascii()) {
        return Err(HttpError::new(400, "non-ASCII request header"));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| HttpError::new(400, "invalid request header"))?;
    let mut lines = text[..text.len() - 4].split("\r\n");
    let first = lines.next().unwrap_or_default();
    if first.bytes().any(|byte| byte < b' ' || byte == 127) {
        return Err(HttpError::new(400, "request line controls unsupported"));
    }
    let mut words = first.split(' ');
    let method = words.next().unwrap_or_default();
    let target = words.next().unwrap_or_default();
    if words.next() != Some("HTTP/1.1") || words.next().is_some() {
        return Err(HttpError::new(400, "HTTP/1.1 request line required"));
    }
    if !target.starts_with('/')
        || target.starts_with("//")
        || target
            .bytes()
            .any(|byte| byte <= b' ' || byte == 127 || matches!(byte, b'#' | b'\\'))
    {
        return Err(HttpError::new(400, "origin-form target required"));
    }
    let method = match method {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        _ => return Err(HttpError::new(405, "method unsupported").with_allow("GET, POST")),
    };
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| HttpError::new(400, "invalid header line"))?;
        if name.is_empty() || !name.bytes().all(header_name_byte) {
            return Err(HttpError::new(400, "invalid header name"));
        }
        if value.bytes().any(|byte| byte < b' ' || byte == 127) {
            return Err(HttpError::new(400, "header controls unsupported"));
        }
        let name = name.to_ascii_lowercase();
        if headers.contains_key(&name) && security_header(&name) {
            return Err(HttpError::new(400, "duplicate security header"));
        }
        match name.as_str() {
            "transfer-encoding" => {
                return Err(HttpError::new(400, "transfer encoding unsupported"));
            }
            "expect" => return Err(HttpError::new(417, "expectation unsupported")),
            "upgrade" => return Err(HttpError::new(400, "protocol upgrade unsupported")),
            "connection"
                if value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade")) =>
            {
                return Err(HttpError::new(400, "protocol upgrade unsupported"));
            }
            _ => {}
        }
        headers.insert(name, value.trim_matches(' ').to_owned());
    }
    if !headers.get("host").is_some_and(|host| !host.is_empty()) {
        return Err(HttpError::new(400, "Host header required"));
    }
    Ok(HttpRequest {
        method,
        target: target.to_owned(),
        headers,
        body: Vec::new(),
    })
}

pub(super) fn content_length(request: &HttpRequest) -> Result<usize, HttpError> {
    let Some(value) = request.header("content-length") else {
        return match request.method {
            HttpMethod::Get => Ok(0),
            HttpMethod::Post => Err(HttpError::new(411, "Content-Length required")),
        };
    };
    if value.is_empty() {
        return Err(HttpError::new(400, "invalid Content-Length"));
    }
    let mut length = 0usize;
    for byte in value.bytes() {
        if !byte.is_ascii_digit() {
            return Err(HttpError::new(400, "invalid Content-Length"));
        }
        length = length
            .checked_mul(10)
            .and_then(|length| length.checked_add(usize::from(byte - b'0')))
            .ok_or_else(|| HttpError::new(400, "Content-Length overflow"))?;
    }
    if length > BODY_LIMIT {
        return Err(HttpError::new(413, "body exceeds limit"));
    }
    if request.method == HttpMethod::Get && length != 0 {
        return Err(HttpError::new(400, "GET body unsupported"));
    }
    Ok(length)
}

fn security_header(name: &str) -> bool {
    matches!(
        name,
        "host" | "content-length" | "origin" | "x-board-token" | "content-type"
    )
}

fn header_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}
