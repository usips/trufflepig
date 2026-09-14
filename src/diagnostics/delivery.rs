use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryFailure {
    BrokenPipe,
    WriteZero,
    Write,
    Flush,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeliveryReceipt {
    pub tokenizer: String,
    pub prepared_tokens: usize,
    pub prepared_bytes: usize,
    pub accepted_bytes: usize,
    pub complete: bool,
    pub completed_unix_micros: Option<u64>,
    pub failure: Option<DeliveryFailure>,
}

/// Measures the exact UTF-8 response and bytes accepted by this stdout writer.
/// A successful flush is required before any identity counts as viewed.
pub fn emit_response(writer: &mut impl Write, response: &str) -> Result<DeliveryReceipt> {
    let mut receipt = DeliveryReceipt {
        tokenizer: "o200k_base".into(),
        prepared_tokens: tiktoken_rs::o200k_base()?.encode_ordinary(response).len(),
        prepared_bytes: response.len(),
        accepted_bytes: 0,
        complete: false,
        completed_unix_micros: None,
        failure: None,
    };
    let bytes = response.as_bytes();
    while receipt.accepted_bytes < bytes.len() {
        match writer.write(&bytes[receipt.accepted_bytes..]) {
            Ok(0) => {
                receipt.failure = Some(DeliveryFailure::WriteZero);
                return Ok(receipt);
            }
            Ok(count) => receipt.accepted_bytes += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                receipt.failure = Some(if error.kind() == io::ErrorKind::BrokenPipe {
                    DeliveryFailure::BrokenPipe
                } else {
                    DeliveryFailure::Write
                });
                return Ok(receipt);
            }
        }
    }
    match writer.flush() {
        Ok(()) => {
            receipt.complete = true;
            receipt.completed_unix_micros = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros()
                    .min(u64::MAX as u128) as u64,
            );
        }
        Err(error) => {
            receipt.failure = Some(if error.kind() == io::ErrorKind::BrokenPipe {
                DeliveryFailure::BrokenPipe
            } else {
                DeliveryFailure::Flush
            });
        }
    }
    Ok(receipt)
}
