use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

use super::DaemonReplyTimeout;

pub(super) const REQUEST_LIMIT: usize = 256 * 1024;
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const ARGUMENT_LIMIT: usize = 256;

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", content = "arguments", deny_unknown_fields)]
pub(super) enum DaemonRequest {
    Arguments {
        context: crate::diagnostics::RequestContext,
        args: Vec<String>,
    },
    Stop,
}

impl DaemonRequest {
    pub(super) fn validate(&self) -> Result<()> {
        if let Self::Arguments { context, args } = self {
            uuid::Uuid::parse_str(&context.request_id).context("invalid request UUID")?;
            ensure!(args.len() <= ARGUMENT_LIMIT, "too many daemon arguments");
            let bytes = args
                .iter()
                .try_fold(0_usize, |total, arg| total.checked_add(arg.len()));
            ensure!(
                bytes.is_some_and(|bytes| bytes <= REQUEST_LIMIT),
                "daemon arguments too large"
            );
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum DaemonReply {
    Success { output: String },
    Failure { message: String },
}

impl DaemonReply {
    pub(super) fn from_result(result: Result<String>) -> Self {
        match result {
            Ok(output) => Self::Success { output },
            Err(error) => Self::Failure {
                message: if super::deadline::is_timed_out(&error) {
                    format!("{error:#}")
                } else {
                    DaemonReplyTimeout::from_error(&error)
                        .map(ToString::to_string)
                        .unwrap_or_else(|| format!("{error:#}"))
                },
            },
        }
    }

    pub(super) fn into_result(self) -> Result<String> {
        match self {
            Self::Success { output } => Ok(output),
            Self::Failure { message } => match DaemonReplyTimeout::from_message(&message) {
                Some(failure) => Err(failure.into()),
                None => {
                    let error = anyhow::Error::msg(message);
                    if super::deadline::is_timed_out(&error) {
                        Err(error)
                    } else {
                        bail!("daemon: {error}")
                    }
                }
            },
        }
    }
}

fn read_frame(reader: &mut impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut header = [0_u8; 4];
    reader
        .read_exact(&mut header)
        .context("read daemon frame length")?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(length <= limit, "daemon frame exceeds {limit} bytes");
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).context("read daemon frame")?;
    Ok(bytes)
}

fn write_frame(writer: &mut impl Write, bytes: &[u8], limit: usize) -> Result<()> {
    ensure!(bytes.len() <= limit, "daemon frame exceeds {limit} bytes");
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

pub(super) fn read_request(reader: &mut impl Read) -> Result<DaemonRequest> {
    let request: DaemonRequest = serde_json::from_slice(&read_frame(reader, REQUEST_LIMIT)?)
        .context("decode daemon request")?;
    request.validate()?;
    Ok(request)
}

pub(super) fn write_request(writer: &mut impl Write, request: &DaemonRequest) -> Result<()> {
    request.validate()?;
    write_frame(writer, &serde_json::to_vec(request)?, REQUEST_LIMIT)
}

pub(super) fn read_reply(reader: &mut impl Read) -> Result<DaemonReply> {
    serde_json::from_slice(&read_frame(reader, RESPONSE_LIMIT)?).context("decode daemon response")
}

pub(super) fn write_reply(writer: &mut impl Write, reply: &DaemonReply) -> Result<()> {
    let oversized = match reply {
        DaemonReply::Success { output } => output.len() > RESPONSE_LIMIT,
        DaemonReply::Failure { message } => message.len() > RESPONSE_LIMIT,
    };
    let bytes = if oversized {
        Vec::new()
    } else {
        serde_json::to_vec(reply)?
    };
    if oversized || bytes.len() > RESPONSE_LIMIT {
        let reply = DaemonReply::Failure {
            message: "daemon response exceeds 4 MiB".to_owned(),
        };
        return write_frame(writer, &serde_json::to_vec(&reply)?, RESPONSE_LIMIT);
    }
    write_frame(writer, &bytes, RESPONSE_LIMIT)
}

#[cfg(test)]
mod serialized_entry_transport_tests {
    use super::*;
    use crate::board::board_vocabulary::ENTRY_TEXT_LIMIT;

    #[test]
    fn maximum_escaped_entry_fits_daemon_request_frame() {
        let text = "\u{1}".repeat(ENTRY_TEXT_LIMIT);
        let request = DaemonRequest::Arguments {
            context: crate::diagnostics::RequestContext {
                request_id: "00000000-0000-4000-8000-000000000000".to_owned(),
                created_unix_micros: 1,
                session: None,
                client: None,
                diagnostic_session: None,
                origin: None,
            },
            args: vec![
                "board".into(),
                "post".into(),
                "P1".into(),
                "note".into(),
                text.clone(),
            ],
        };
        let mut frame = Vec::with_capacity(4 + 6 * ENTRY_TEXT_LIMIT + 128);
        write_request(&mut frame, &request).unwrap();
        let length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;

        assert!(length > 64 * 1024, "fixture must exercise the raised bound");
        assert!(
            length <= REQUEST_LIMIT,
            "serialized request exceeds its frame limit"
        );
        assert_eq!(length, frame.len() - 4);
        let decoded = read_request(&mut frame.as_slice()).unwrap();
        let DaemonRequest::Arguments { args, .. } = decoded else {
            panic!("expected argument request");
        };
        assert_eq!(args.last().map(String::as_str), Some(text.as_str()));
    }
}
