use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub(super) const REQUEST_LIMIT: usize = 64 * 1024;
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const ARGUMENT_LIMIT: usize = 256;

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", content = "arguments", deny_unknown_fields)]
pub(super) enum DaemonRequest {
    Arguments(Vec<String>),
    Stop,
}

impl DaemonRequest {
    pub(super) fn validate(&self) -> Result<()> {
        if let Self::Arguments(args) = self {
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
