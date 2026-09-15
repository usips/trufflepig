//! Bounded uncached semantic throughput benchmark used by the GPU gate.
//!
//! The Python harness owns device discovery and memory sampling. This process
//! owns the model session and emits one JSON document for either provider.

#[cfg(feature = "semantic")]
mod benchmark_inputs;
#[cfg(feature = "semantic")]
mod benchmark_scenarios;

#[cfg(feature = "semantic")]
use benchmark_inputs::{load_inputs, mixed_inputs, parse_args, take_bytes};
#[cfg(feature = "semantic")]
use benchmark_scenarios::{
    run_batches, run_long_probe, synthetic_batch, synthetic_tokens, with_shape,
};
#[cfg(feature = "semantic")]
use std::time::Instant;

#[cfg(feature = "semantic")]
const MAX_REGION_BYTES: usize = 4096;
#[cfg(feature = "semantic")]
const MAX_INPUT_TOKENS: usize = 4096;
#[cfg(feature = "semantic")]
const MAX_BATCH_PADDED_TOKENS: usize = 8192;

#[cfg(not(feature = "semantic"))]
fn main() {
    eprintln!("semantic_benchmark requires --features semantic (or semantic-cuda)");
    std::process::exit(2);
}

#[cfg(feature = "semantic")]
fn main() {
    match run() {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string(&report).expect("report serializes")
            );
            if report["status"] != "passed" {
                std::process::exit(3);
            }
        }
        Err(error) => {
            let report = serde_json::json!({
                "status": "unavailable",
                "error": error.to_string(),
                "scope": "bounded uncached semantic benchmark"
            });
            println!(
                "{}",
                serde_json::to_string(&report).expect("report serializes")
            );
            std::process::exit(2);
        }
    }
}

#[cfg(feature = "semantic")]
fn run() -> anyhow::Result<serde_json::Value> {
    let args = parse_args()?;
    let inputs = load_inputs(&args.inputs)?;
    let started = Instant::now();
    let provider = match args.provider.as_str() {
        "cpu" => trufflepig::semantic::SemanticProviderConfig::cpu(),
        "cuda" => trufflepig::semantic::SemanticProviderConfig::cuda(args.ordinal),
        value => anyhow::bail!("provider must be cpu or cuda, got {value}"),
    }
    .with_arena_max_bytes(args.arena_bytes);
    let mut engine = trufflepig::semantic::SemanticInferenceEngine::open_with_runtime(
        &args.model_dir,
        provider,
        &args.runtime,
    )?;
    let initialize_ms = started.elapsed().as_millis();
    let source_texts = inputs
        .iter()
        .map(|input| input.text.as_str())
        .collect::<Vec<_>>();
    let short_inputs = source_texts[..8]
        .iter()
        .map(|text| take_bytes(text, 512))
        .collect::<Vec<_>>();

    let mut scenarios = Vec::new();
    scenarios.push(run_batches(
        &mut engine,
        "safe_8xshort",
        &short_inputs,
        args.iterations,
    ));
    scenarios.push(run_batches(
        &mut engine,
        "mixed_length",
        &mixed_inputs(&inputs),
        args.iterations,
    ));
    // These synthetic shapes use the pinned tokenizer's three special tokens
    // so their model lengths are exact while remaining bounded by the public
    // 4096-token per-input contract.
    scenarios.push(with_shape(
        run_batches(
            &mut engine,
            "exact_1x4096_singleton",
            &[synthetic_tokens(4096)],
            args.iterations,
        ),
        &[4096],
    ));
    scenarios.push(with_shape(
        run_batches(
            &mut engine,
            "exact_2x4096_padded8192",
            &synthetic_batch(&[4096, 4096]),
            args.iterations,
        ),
        &[4096, 4096],
    ));
    scenarios.push(with_shape(
        run_batches(
            &mut engine,
            "exact_8x1024_padded8192",
            &synthetic_batch(&[1024; 8]),
            args.iterations,
        ),
        &[1024; 8],
    ));
    let mixed = run_batches(
        &mut engine,
        "mixed_1024_4096_padded8192",
        &synthetic_batch(&[4096, 1024]),
        args.iterations,
    );
    scenarios.push(with_shape(mixed, &[4096, 1024]));
    scenarios.push(run_batches(
        &mut engine,
        "generic_8_inputs_realistic",
        &source_texts[..8],
        args.iterations,
    ));
    scenarios.push(run_batches(
        &mut engine,
        "frozen_256_regions",
        &source_texts,
        args.iterations,
    ));
    scenarios.push(run_batches(
        &mut engine,
        "repeated_batch",
        &source_texts[..8],
        args.iterations.saturating_mul(2).max(2),
    ));
    scenarios.push(if args.include_long {
        run_long_probe(&mut engine)
    } else {
        serde_json::json!({
            "name": "8192_tokens_singleton",
            "status": "skipped",
            "requested_token_count": 8192,
            "token_count": null,
            "reason": "explicit --include-long is required; long-shape memory can exceed 16 GiB"
        })
    });

    let failed = scenarios
        .iter()
        .any(|scenario| matches!(scenario["status"].as_str(), Some("failed" | "error")));
    let incomplete = scenarios
        .iter()
        .any(|scenario| scenario["status"] == "skipped");
    let checksum = scenarios
        .iter()
        .filter_map(|scenario| scenario["checksum"].as_u64())
        .fold(0_u64, u64::wrapping_add);
    Ok(serde_json::json!({
        "status": if failed { "failed" } else if incomplete { "incomplete" } else { "passed" },
        "provider": args.provider.to_uppercase(),
        "device_ordinal": if args.provider == "cuda" { serde_json::Value::from(args.ordinal) } else { serde_json::Value::Null },
        "arena_max_bytes": args.arena_bytes,
        "initialize_ms": initialize_ms,
        "total_ms": started.elapsed().as_millis(),
        "iterations": args.iterations,
        "input_count": inputs.len(),
        "no_cache": true,
        "max_region_bytes": MAX_REGION_BYTES,
        "max_batch_inputs": 8,
        "max_input_tokens": MAX_INPUT_TOKENS,
        "max_batch_padded_tokens": MAX_BATCH_PADDED_TOKENS,
        "checksum": checksum,
        "scenarios": scenarios,
        "scope": "actual SemanticInferenceEngine::embed_batch_uncached calls; throughput excludes model initialization"
    }))
}
