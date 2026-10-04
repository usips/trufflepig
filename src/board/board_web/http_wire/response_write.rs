use std::{
    io::{self, Write},
    net::TcpStream,
    time::{Duration, Instant},
};

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const SECURITY_HEADERS: &str = concat!(
    "X-Content-Type-Options: nosniff\r\n",
    "Referrer-Policy: no-referrer\r\n",
    "Content-Security-Policy: default-src 'self'; frame-ancestors 'none'; ",
    "base-uri 'none'; object-src 'none'; form-action 'none'\r\n"
);

struct ResponseHeaders<'a> {
    status: u16,
    content_type: &'a str,
    private: bool,
    retry_after_seconds: Option<u32>,
    allow: Option<&'a str>,
}

impl ResponseHeaders<'_> {
    fn unavailable(retry_after_seconds: u32) -> Self {
        Self {
            status: 503,
            content_type: "application/json",
            private: true,
            retry_after_seconds: Some(retry_after_seconds),
            allow: None,
        }
    }
}

pub(crate) fn send_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    private: bool,
) -> io::Result<()> {
    send_response_until(
        stream,
        status,
        content_type,
        body,
        private,
        Instant::now() + WRITE_TIMEOUT,
    )
}

pub(super) fn send_response_until(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    private: bool,
    deadline: Instant,
) -> io::Result<()> {
    let headers = ResponseHeaders {
        status,
        content_type,
        private,
        retry_after_seconds: None,
        allow: None,
    };
    write_response(stream, headers, body, deadline)
}

/// 405 replies list the methods the route permits.
pub(crate) fn send_method_refusal(
    stream: &mut TcpStream,
    body: &[u8],
    allow: &str,
) -> io::Result<()> {
    let headers = ResponseHeaders {
        status: 405,
        content_type: "application/json",
        private: true,
        retry_after_seconds: None,
        allow: Some(allow),
    };
    write_response(stream, headers, body, Instant::now() + WRITE_TIMEOUT)
}

pub(crate) fn send_unavailable(
    stream: &mut TcpStream,
    retry_after_seconds: u32,
    body: &[u8],
) -> io::Result<()> {
    let headers = ResponseHeaders::unavailable(retry_after_seconds);
    write_response(stream, headers, body, Instant::now() + WRITE_TIMEOUT)
}

/// Precompute once for the accept loop's single nonblocking busy-response write.
pub(crate) fn unavailable_response(retry_after_seconds: u32, body: &[u8]) -> Vec<u8> {
    let header = encode_headers(
        ResponseHeaders::unavailable(retry_after_seconds),
        body.len(),
    )
    .expect("fixed JSON response headers are valid");
    let mut response = Vec::with_capacity(header.len() + body.len());
    response.extend_from_slice(header.as_bytes());
    response.extend_from_slice(body);
    response
}

fn write_response(
    stream: &mut TcpStream,
    headers: ResponseHeaders<'_>,
    body: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    let header = encode_headers(headers, body.len())?;
    write_before(stream, header.as_bytes(), deadline)?;
    write_before(stream, body, deadline)
}

fn encode_headers(headers: ResponseHeaders<'_>, content_length: usize) -> io::Result<String> {
    let ResponseHeaders {
        status,
        content_type,
        private,
        retry_after_seconds,
        allow,
    } = headers;
    if content_type.is_empty() || content_type.bytes().any(|byte| !(32..127).contains(&byte)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid content type",
        ));
    }
    if allow.is_some_and(|allow| {
        allow.is_empty() || allow.bytes().any(|byte| !(32..127).contains(&byte))
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid allow list",
        ));
    }
    let cache = if private {
        "Cache-Control: no-store\r\n"
    } else {
        ""
    };
    let retry_after = retry_after_seconds
        .map(|seconds| format!("Retry-After: {seconds}\r\n"))
        .unwrap_or_default();
    let allow = allow
        .map(|methods| format!("Allow: {methods}\r\n"))
        .unwrap_or_default();
    Ok(format!(
        concat!(
            "HTTP/1.1 {status} {}\r\n",
            "Content-Type: {content_type}\r\n",
            "Content-Length: {content_length}\r\n",
            "Connection: close\r\n",
            "{cache}{retry_after}{allow}{SECURITY_HEADERS}\r\n"
        ),
        reason(status),
        status = status,
        content_type = content_type,
        content_length = content_length,
        cache = cache,
        retry_after = retry_after,
        allow = allow,
        SECURITY_HEADERS = SECURITY_HEADERS,
    ))
}

fn write_before(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "response deadline exceeded"))?;
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "response peer closed",
                ));
            }
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "response deadline exceeded",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "response deadline exceeded",
        ));
    }
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        421 => "Misdirected Request",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}
