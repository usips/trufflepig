//! Bounded, single-request HTTP/1.1 transport for the local board server.
//! Acceptance time includes queueing; every connection closes after one response.

mod request_parse;
mod response_write;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    io::{self, Read},
    net::TcpStream,
    time::{Duration, Instant},
};

pub(crate) use response_write::{
    begin_event_stream, send_response, send_unavailable, unavailable_response, write_event_bytes,
};

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const HEADER_LIMIT: usize = 16 * 1024;
pub(crate) const BODY_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HttpMethod {
    Get,
    Post,
}

#[derive(Debug)]
pub(crate) struct HttpRequest {
    pub(crate) method: HttpMethod,
    pub(crate) target: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Vec<u8>,
}

impl HttpRequest {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn path(&self) -> &str {
        self.target
            .split_once('?')
            .map_or(&self.target, |(path, _)| path)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HttpError {
    pub(crate) status: u16,
    pub(crate) message: &'static str,
}

impl HttpError {
    pub(crate) const fn new(status: u16, message: &'static str) -> Self {
        Self { status, message }
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} {}", self.status, self.message)
    }
}

impl std::error::Error for HttpError {}

pub(crate) fn read_request(
    stream: &mut TcpStream,
    accepted_at: Instant,
) -> Result<HttpRequest, HttpError> {
    read_request_until(stream, accepted_at + REQUEST_TIMEOUT)
}

pub(crate) fn read_request_until(
    stream: &mut TcpStream,
    deadline: Instant,
) -> Result<HttpRequest, HttpError> {
    let mut buffered = Vec::with_capacity(4096);
    let header_end = loop {
        remaining(deadline)?;
        if let Some(end) = buffered.windows(4).position(|part| part == b"\r\n\r\n") {
            let end = end + 4;
            if end > HEADER_LIMIT {
                return Err(HttpError::new(431, "headers exceed limit"));
            }
            break end;
        }
        request_parse::check_line_endings(&buffered)?;
        if buffered.len() >= HEADER_LIMIT {
            return Err(HttpError::new(431, "headers exceed limit"));
        }
        let mut chunk = [0; 4096];
        let available = (HEADER_LIMIT - buffered.len()).min(chunk.len());
        let count = read_before(stream, &mut chunk[..available], deadline)?;
        buffered.extend_from_slice(&chunk[..count]);
    };
    let mut request = request_parse::parse_headers(&buffered[..header_end])?;
    let content_length = request_parse::content_length(&request)?;
    let received_body = &buffered[header_end..];
    if received_body.len() > content_length {
        return Err(HttpError::new(400, "pipelined bytes are unsupported"));
    }
    request.body = Vec::with_capacity(content_length);
    request.body.extend_from_slice(received_body);
    while request.body.len() < content_length {
        let mut chunk = [0; 4096];
        let available = (content_length - request.body.len()).min(chunk.len());
        let count = read_before(stream, &mut chunk[..available], deadline)?;
        request.body.extend_from_slice(&chunk[..count]);
    }
    remaining(deadline)?;
    reject_queued_bytes(stream)?;
    remaining(deadline)?;
    Ok(request)
}

fn remaining(deadline: Instant) -> Result<Duration, HttpError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| HttpError::new(408, "request deadline exceeded"))
}

fn read_before(
    stream: &mut TcpStream,
    bytes: &mut [u8],
    deadline: Instant,
) -> Result<usize, HttpError> {
    loop {
        stream
            .set_read_timeout(Some(remaining(deadline)?))
            .map_err(|_| HttpError::new(400, "cannot read request"))?;
        match stream.read(bytes) {
            Ok(0) => return Err(HttpError::new(400, "incomplete request")),
            Ok(count) => {
                remaining(deadline)?;
                return Ok(count);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(HttpError::new(408, "request deadline exceeded"));
            }
            Err(_) => return Err(HttpError::new(400, "cannot read request")),
        }
    }
}

fn reject_queued_bytes(stream: &TcpStream) -> Result<(), HttpError> {
    stream
        .set_nonblocking(true)
        .map_err(|_| HttpError::new(400, "cannot inspect request"))?;
    let inspected = stream.peek(&mut [0]);
    stream
        .set_nonblocking(false)
        .map_err(|_| HttpError::new(400, "cannot inspect request"))?;
    match inspected {
        Ok(0) => Ok(()),
        Ok(_) => Err(HttpError::new(400, "pipelined bytes are unsupported")),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Err(_) => Err(HttpError::new(400, "cannot inspect request")),
    }
}
