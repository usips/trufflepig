use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

const INPUT_COUNT: usize = 256;
const MAX_REGION_BYTES: usize = 4096;
const ARENA_BYTES: u64 = 10 * 1024 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct FrozenInputs {
    inputs: Vec<FrozenRegion>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FrozenRegion {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) sha256: String,
    pub(crate) text: String,
}

#[derive(Debug)]
pub(crate) struct Args {
    pub(crate) model_dir: PathBuf,
    pub(crate) runtime: PathBuf,
    pub(crate) inputs: PathBuf,
    pub(crate) provider: String,
    pub(crate) ordinal: i32,
    pub(crate) iterations: usize,
    pub(crate) arena_bytes: u64,
    pub(crate) include_long: bool,
}

pub(crate) fn parse_args() -> anyhow::Result<Args> {
    let mut model_dir = None;
    let mut runtime = None;
    let mut inputs = None;
    let mut provider = None;
    let mut ordinal = 0;
    let mut iterations = 1;
    let mut arena_bytes = ARENA_BYTES;
    let mut include_long = false;
    let mut values = env::args().skip(1);
    while let Some(flag) = values.next() {
        let value = |values: &mut std::iter::Skip<env::Args>, flag: &str| {
            values
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"))
        };
        match flag.as_str() {
            "--model-dir" => model_dir = Some(PathBuf::from(value(&mut values, &flag)?)),
            "--runtime" => runtime = Some(PathBuf::from(value(&mut values, &flag)?)),
            "--inputs" => inputs = Some(PathBuf::from(value(&mut values, &flag)?)),
            "--provider" => provider = Some(value(&mut values, &flag)?),
            "--device-ordinal" => ordinal = value(&mut values, &flag)?.parse()?,
            "--iterations" => iterations = value(&mut values, &flag)?.parse()?,
            "--arena-max-bytes" => arena_bytes = value(&mut values, &flag)?.parse()?,
            "--include-long" => include_long = true,
            "--help" => anyhow::bail!(
                "semantic_benchmark --model-dir DIR --runtime LIB --inputs JSON --provider cpu|cuda [--device-ordinal N]"
            ),
            value => anyhow::bail!("unknown argument {value}"),
        }
    }
    Ok(Args {
        model_dir: model_dir.ok_or_else(|| anyhow::anyhow!("--model-dir is required"))?,
        runtime: runtime.ok_or_else(|| anyhow::anyhow!("--runtime is required"))?,
        inputs: inputs.ok_or_else(|| anyhow::anyhow!("--inputs is required"))?,
        provider: provider.ok_or_else(|| anyhow::anyhow!("--provider is required"))?,
        ordinal,
        iterations: iterations.max(1),
        arena_bytes,
        include_long,
    })
}

pub(crate) fn load_inputs(path: &PathBuf) -> anyhow::Result<Vec<FrozenRegion>> {
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let parsed = if value.is_array() {
        serde_json::from_value::<Vec<FrozenRegion>>(value)?
    } else {
        serde_json::from_value::<FrozenInputs>(value)?.inputs
    };
    if parsed.len() != INPUT_COUNT {
        anyhow::bail!(
            "input manifest must contain exactly {INPUT_COUNT} regions, got {}",
            parsed.len()
        );
    }
    let mut hashes = std::collections::HashSet::with_capacity(parsed.len());
    for input in &parsed {
        let bytes = input.text.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_REGION_BYTES {
            anyhow::bail!(
                "{} has {} bytes; expected 1..{MAX_REGION_BYTES}",
                input.id,
                bytes.len()
            );
        }
        if input.end < input.start || input.end - input.start != bytes.len() {
            anyhow::bail!("{} has inconsistent source offsets", input.id);
        }
        let mut digest = Sha256::new();
        digest.update(bytes);
        let digest = format!("{:x}", digest.finalize());
        if digest != input.sha256 || !hashes.insert(input.sha256.clone()) {
            anyhow::bail!("{} is not distinct or has an invalid sha256", input.id);
        }
        if input.path.is_empty() {
            anyhow::bail!("{} has no source path", input.id);
        }
    }
    Ok(parsed)
}

pub(crate) fn take_bytes(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    text.char_indices()
        .take_while(|(index, _)| *index < limit)
        .map(|(_, character)| character)
        .collect()
}

pub(crate) fn mixed_inputs(inputs: &[FrozenRegion]) -> Vec<String> {
    const LIMITS: [usize; 8] = [128, 512, 1024, 2048, 256, 768, 1536, 3072];
    inputs[..8]
        .iter()
        .zip(LIMITS)
        .map(|(input, limit)| take_bytes(&input.text, limit))
        .collect()
}
