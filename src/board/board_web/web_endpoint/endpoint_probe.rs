//! Listener ownership probe: challenge the endpoint and verify its proof.
//! Posts a fresh nonce to the unauthenticated challenge route and checks the
//! token-keyed HMAC, so callers never send the token itself.

use super::http_wire;
use crate::board::{
    board_protocol::BOARD_API,
    board_web::web_guard::{ChallengeNonce, WebGuard},
};
use anyhow::{Context, Result, ensure};
use std::{
    io::{self, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};

/// Verify the listener holds our token without ever sending it: POST a random
/// nonce to the unauthenticated challenge route and check the HMAC proof.
pub(super) fn probe(address: SocketAddr, guard: &WebGuard, expires: Instant) -> Result<()> {
    let nonce = ChallengeNonce::generate().context("board_unavailable: web challenge failed")?;
    let mut socket = TcpStream::connect_timeout(&address, remaining(expires)?)?;
    let body = serde_json::to_vec(&serde_json::json!({
        "api": BOARD_API,
        "nonce": nonce.to_hex(),
    }))?;
    let headers = format!(
        concat!(
            "POST /api/v1/challenge HTTP/1.1\r\n",
            "Host: {}\r\n",
            "Origin: {}\r\n",
            "Content-Type: application/json\r\n",
            "Content-Length: {}\r\n",
            "Connection: close\r\n\r\n"
        ),
        guard.authority(),
        guard.origin(),
        body.len(),
    );
    write_before(&mut socket, headers.as_bytes(), expires)?;
    write_before(&mut socket, &body, expires)?;
    let reply = read_response(&mut socket, expires)?;
    let value: serde_json::Value = serde_json::from_slice(&reply)?;
    ensure!(
        value["api"].as_u64() == Some(u64::from(BOARD_API)),
        "board_unavailable: web listener answered with an incompatible api version"
    );
    let proof = value["proof"]
        .as_str()
        .context("board_unavailable: web listener answered without an ownership proof")?;
    ensure!(
        guard.challenge_matches(&nonce, proof),
        "board_unavailable: web listener failed the ownership challenge"
    );
    Ok(())
}

fn remaining(expires: Instant) -> io::Result<Duration> {
    let duration = expires.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "web listener probe timed out",
        ));
    }
    Ok(duration)
}

fn write_before(socket: &mut TcpStream, mut bytes: &[u8], expires: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        socket.set_write_timeout(Some(remaining(expires)?))?;
        let count = socket.write(bytes)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "web listener closed",
            ));
        }
        bytes = &bytes[count..];
    }
    Ok(())
}

fn read_response(socket: &mut TcpStream, expires: Instant) -> Result<Vec<u8>> {
    let mut buffered = Vec::with_capacity(4096);
    let header_end = loop {
        if let Some(start) = buffered.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break start + 4;
        }
        ensure!(
            buffered.len() < http_wire::HEADER_LIMIT,
            "web response headers too large"
        );
        read_chunk(socket, &mut buffered, expires)?;
    };
    ensure!(
        header_end <= http_wire::HEADER_LIMIT,
        "web response headers too large"
    );
    let headers = std::str::from_utf8(&buffered[..header_end])?;
    let mut lines = headers.split("\r\n");
    ensure!(
        lines
            .next()
            .is_some_and(|line| line.starts_with("HTTP/1.1 200 ")),
        "web listener rejected the ownership challenge"
    );
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .context("invalid web response header")?;
        ensure!(
            !name.eq_ignore_ascii_case("transfer-encoding"),
            "unexpected streaming web response"
        );
        if name.eq_ignore_ascii_case("content-length") {
            ensure!(length.is_none(), "duplicate web response length");
            let value = value.trim();
            ensure!(
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
                "invalid web response length"
            );
            length = Some(value.parse::<usize>()?);
        }
    }
    let length = length.context("web response length required")?;
    ensure!(
        length <= http_wire::BODY_LIMIT,
        "web response body too large"
    );
    let end = header_end + length;
    while buffered.len() < end {
        read_chunk(socket, &mut buffered, expires)?;
    }
    ensure!(
        buffered.len() == end,
        "unexpected trailing web response bytes"
    );
    Ok(buffered[header_end..].to_vec())
}

fn read_chunk(socket: &mut TcpStream, buffered: &mut Vec<u8>, expires: Instant) -> Result<()> {
    socket.set_read_timeout(Some(remaining(expires)?))?;
    let mut chunk = [0; 4096];
    let count = http_wire::read_ignoring_interrupts(socket, &mut chunk)?;
    ensure!(count > 0, "web listener closed before readiness reply");
    buffered.extend_from_slice(&chunk[..count]);
    Ok(())
}
