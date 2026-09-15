use super::scheduler::RequestClass;
use crate::semantic::runtime_config::InferenceConfig;
use crate::semantic::{
    MODEL_NAME, MODEL_REVISION, RERANKER_NAME, RERANKER_REVISION, check_rerank_bounds,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{ErrorKind, Read, Write},
    time::Instant,
};

pub const PROTOCOL_VERSION: u32 = 1;
pub const FRAME_LIMIT: usize = 1024 * 1024;
pub const MAX_INPUTS: usize = 8;
pub const MAX_RAW_INPUT_BYTES: usize = 8 * 1024 * 1024;
const BUILD_ID: &str = env!("TRUFFLEPIG_BUILD_ID");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handshake {
    pub protocol: u32,
    pub build: String,
    pub config: String,
    pub model: String,
}

impl Handshake {
    pub fn for_config(config: &InferenceConfig) -> Self {
        let model_path = config
            .model_dir
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let rerank_model_path = config
            .rerank_model_dir
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let model = format!(
            "{MODEL_NAME}:{MODEL_REVISION}:{model_path}:{RERANKER_NAME}:{RERANKER_REVISION}:{rerank_model_path}"
        );
        Self {
            protocol: PROTOCOL_VERSION,
            build: BUILD_ID.into(),
            config: config.fingerprint(),
            model: blake3::hash(model.as_bytes()).to_hex().to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRequest {
    pub handshake: Handshake,
    pub command: WorkerCommand,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum WorkerCommand {
    Status,
    Stop,
    Embed {
        class: RequestClass,
        root_id: String,
        texts: Vec<String>,
        deadline_ms: u64,
    },
    Rerank {
        root_id: String,
        query: String,
        documents: Vec<String>,
        deadline_ms: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum WorkerReply {
    Status(super::WorkerStatus),
    Embeddings { values: Vec<Vec<f32>> },
    Scores { values: Vec<f32> },
    Error { message: String },
}

impl WorkerRequest {
    pub fn new(config: &InferenceConfig, command: WorkerCommand) -> Self {
        Self {
            handshake: Handshake::for_config(config),
            command,
        }
    }
}

pub fn read_frame(reader: &mut impl Read) -> Result<Vec<u8>> {
    let mut header = [0_u8; 4];
    reader
        .read_exact(&mut header)
        .context("read worker frame length")?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(length <= FRAME_LIMIT, "worker frame exceeds 1 MiB");
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes).context("read worker frame")?;
    Ok(bytes)
}

/// Reads one frame while enforcing one absolute deadline across every read.
/// Socket callers use a short read timeout so trickled clients cannot reset it.
pub fn read_frame_with_deadline(reader: &mut impl Read, deadline: Instant) -> Result<Vec<u8>> {
    let mut header = [0_u8; 4];
    read_exact_until(reader, &mut header, deadline).context("read worker frame length")?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(length <= FRAME_LIMIT, "worker frame exceeds 1 MiB");
    let mut bytes = vec![0_u8; length];
    read_exact_until(reader, &mut bytes, deadline).context("read worker frame")?;
    Ok(bytes)
}

pub fn write_frame_with_deadline(
    writer: &mut impl Write,
    bytes: &[u8],
    deadline: Instant,
) -> Result<()> {
    ensure!(bytes.len() <= FRAME_LIMIT, "worker frame exceeds 1 MiB");
    write_all_until(writer, &(bytes.len() as u32).to_be_bytes(), deadline)
        .context("write worker frame length")?;
    write_all_until(writer, bytes, deadline).context("write worker frame")?;
    if Instant::now() >= deadline {
        bail!("worker frame write deadline exceeded");
    }
    writer.flush().context("flush worker frame")?;
    Ok(())
}

fn read_exact_until(reader: &mut impl Read, bytes: &mut [u8], deadline: Instant) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        if Instant::now() >= deadline {
            bail!("worker frame read deadline exceeded");
        }
        match reader.read(&mut bytes[offset..]) {
            Ok(0) => bail!("unexpected end of worker frame"),
            Ok(count) => offset += count,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn write_all_until(writer: &mut impl Write, bytes: &[u8], deadline: Instant) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        if Instant::now() >= deadline {
            bail!("worker frame write deadline exceeded");
        }
        match writer.write(&bytes[offset..]) {
            Ok(0) => bail!("unable to write worker frame"),
            Ok(count) => offset += count,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() <= FRAME_LIMIT, "worker frame exceeds 1 MiB");
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn read_request_with_deadline(
    reader: &mut impl Read,
    deadline: Instant,
) -> Result<WorkerRequest> {
    let request: WorkerRequest =
        serde_json::from_slice(&read_frame_with_deadline(reader, deadline)?)
            .context("decode worker request")?;
    validate_command(&request.command)?;
    Ok(request)
}

pub fn write_request_with_deadline(
    writer: &mut impl Write,
    request: &WorkerRequest,
    deadline: Instant,
) -> Result<()> {
    validate_command(&request.command)?;
    write_frame_with_deadline(writer, &serde_json::to_vec(request)?, deadline)
}

pub fn read_reply_with_deadline(reader: &mut impl Read, deadline: Instant) -> Result<WorkerReply> {
    serde_json::from_slice(&read_frame_with_deadline(reader, deadline)?)
        .context("decode worker reply")
}

pub fn write_reply(writer: &mut impl Write, reply: &WorkerReply) -> Result<()> {
    let bytes = serde_json::to_vec(reply)?;
    if bytes.len() <= FRAME_LIMIT {
        return write_frame(writer, &bytes);
    }
    write_frame(
        writer,
        &serde_json::to_vec(&WorkerReply::Error {
            message: "worker reply exceeds 1 MiB".into(),
        })?,
    )
}

pub fn validate_inputs(texts: &[String]) -> Result<()> {
    ensure!(
        texts.len() <= MAX_INPUTS,
        "semantic_admission: at most 8 inputs per batch"
    );
    ensure!(
        raw_input_bytes(texts) <= MAX_RAW_INPUT_BYTES,
        "semantic_admission: raw inputs exceed 8 MiB"
    );
    Ok(())
}

pub fn raw_input_bytes(texts: &[String]) -> usize {
    texts
        .iter()
        .fold(0_usize, |total, text| total.saturating_add(text.len()))
}

/// Validates rerank admission bounds shared with [`check_rerank_bounds`].
pub fn validate_rerank_inputs(query: &str, documents: &[String]) -> Result<()> {
    check_rerank_bounds(query, documents)
}

fn validate_command(command: &WorkerCommand) -> Result<()> {
    match command {
        WorkerCommand::Embed {
            root_id,
            texts,
            deadline_ms,
            ..
        } => {
            ensure!(!root_id.is_empty(), "semantic_admission: root id is empty");
            ensure!(*deadline_ms != 0, "semantic_admission: deadline is missing");
            validate_inputs(texts)?;
        }
        WorkerCommand::Rerank {
            root_id,
            query,
            documents,
            deadline_ms,
        } => {
            ensure!(!root_id.is_empty(), "rerank_admission: root id is empty");
            ensure!(*deadline_ms != 0, "rerank_admission: deadline is missing");
            validate_rerank_inputs(query, documents)?;
        }
        WorkerCommand::Status | WorkerCommand::Stop => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, os::unix::net::UnixStream, thread, time::Duration};

    #[test]
    fn frames_reject_one_byte_over_one_mib() {
        let mut output = Vec::new();
        assert!(write_frame(&mut output, &vec![0_u8; FRAME_LIMIT + 1]).is_err());
    }

    #[test]
    fn frames_round_trip_binary_payload() -> Result<()> {
        let input = vec![1_u8, 2, 3, 4];
        let mut output = Vec::new();
        write_frame(&mut output, &input)?;
        assert_eq!(read_frame(&mut Cursor::new(output))?, input);
        Ok(())
    }

    #[test]
    fn input_admission_bounds_count_and_bytes() {
        assert!(validate_inputs(&vec!["x".into(); MAX_INPUTS + 1]).is_err());
        assert!(validate_inputs(&["x".repeat(MAX_RAW_INPUT_BYTES + 1)]).is_err());
    }

    #[test]
    fn serialized_embedding_reply_is_well_below_frame_limit() {
        use crate::semantic::DIMENSIONS;
        let values = vec![vec![0.0; DIMENSIONS]; MAX_INPUTS];
        let bytes = serde_json::to_vec(&WorkerReply::Embeddings { values }).unwrap();
        assert!(bytes.len() < FRAME_LIMIT);
    }

    #[test]
    fn serialized_scores_reply_is_well_below_frame_limit() {
        use crate::semantic::RERANK_MAX_DOCUMENTS;
        let values = vec![0.0_f32; RERANK_MAX_DOCUMENTS];
        let bytes = serde_json::to_vec(&WorkerReply::Scores { values }).unwrap();
        assert!(bytes.len() < FRAME_LIMIT);
    }

    #[test]
    fn rerank_request_over_document_limit_fails_validation() {
        use crate::semantic::RERANK_MAX_DOCUMENTS;
        let command = WorkerCommand::Rerank {
            root_id: "root".into(),
            query: "query".into(),
            documents: vec!["doc".into(); RERANK_MAX_DOCUMENTS + 1],
            deadline_ms: 1,
        };
        let error = validate_command(&command).unwrap_err();
        assert!(error.to_string().contains("rerank_admission"));
    }

    #[test]
    fn worst_case_rerank_request_stays_under_frame_limit() -> Result<()> {
        use crate::semantic::RERANK_MAX_DOCUMENTS;
        let request = WorkerRequest {
            handshake: Handshake {
                protocol: PROTOCOL_VERSION,
                build: "x".repeat(64),
                config: "x".repeat(64),
                model: "x".repeat(64),
            },
            command: WorkerCommand::Rerank {
                root_id: "x".repeat(256),
                query: "\"".repeat(4096),
                documents: vec!["\"".repeat(4096); RERANK_MAX_DOCUMENTS],
                deadline_ms: u64::MAX,
            },
        };
        let bytes = serde_json::to_vec(&request)?;
        assert!(bytes.len() < FRAME_LIMIT);
        Ok(())
    }

    #[test]
    fn trickled_reply_hits_one_absolute_deadline() -> Result<()> {
        let (mut writer, mut reader) = UnixStream::pair()?;
        reader.set_read_timeout(Some(Duration::from_millis(5)))?;
        let bytes = serde_json::to_vec(&WorkerReply::Error {
            message: "trickled".into(),
        })?;
        let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
        frame.extend(bytes);
        let writer_thread = thread::spawn(move || {
            for byte in frame {
                if writer.write_all(&[byte]).is_err() || writer.flush().is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        let deadline = Instant::now() + Duration::from_millis(30);
        let error = read_reply_with_deadline(&mut reader, deadline)
            .expect_err("trickled reply must hit its absolute deadline");
        assert!(format!("{error:#}").contains("deadline"));
        drop(reader);
        writer_thread.join().expect("trickle writer");
        Ok(())
    }
}
