use std::time::Instant;

pub(crate) fn synthetic_tokens(tokens: usize) -> String {
    assert!(tokens >= 3, "synthetic shape must fit special tokens");
    // For this pinned tokenizer, each repeated `x ` contributes one model
    // token and the encoder adds three special tokens.
    "x ".repeat(tokens - 3)
}

pub(crate) fn synthetic_batch(lengths: &[usize]) -> Vec<String> {
    lengths.iter().copied().map(synthetic_tokens).collect()
}

pub(crate) fn with_shape(
    mut report: serde_json::Value,
    token_lengths: &[usize],
) -> serde_json::Value {
    let padded_tokens =
        token_lengths.iter().copied().max().unwrap_or_default() * token_lengths.len();
    report["requested_token_lengths"] = serde_json::json!(token_lengths);
    report["requested_padded_tokens"] = serde_json::json!(padded_tokens);
    report
}

pub(crate) fn run_batches<S: AsRef<str>>(
    engine: &mut trufflepig::semantic::SemanticInferenceEngine,
    name: &str,
    inputs: &[S],
    iterations: usize,
) -> serde_json::Value {
    let started = Instant::now();
    let mut calls = 0_usize;
    let mut outputs = 0_usize;
    let mut checksum = 0_u64;
    let mut error = None;
    for _ in 0..iterations {
        for batch in inputs.chunks(8) {
            let texts = batch.iter().map(AsRef::as_ref).collect::<Vec<_>>();
            match engine.embed_batch_uncached(&texts) {
                Ok(embeddings) => {
                    outputs += embeddings.len();
                    checksum = embeddings.iter().fold(checksum, |sum, embedding| {
                        sum.wrapping_add(u64::from(embedding.0[0].to_bits()))
                    });
                }
                Err(failure) => {
                    error = Some(failure.to_string());
                    break;
                }
            }
            calls += 1;
        }
        if error.is_some() {
            break;
        }
    }
    let elapsed_ms = started.elapsed().as_millis();
    serde_json::json!({
        "name": name,
        "status": if error.is_some() { "failed" } else { "passed" },
        "inputs": inputs.len(),
        "batch_size": 8,
        "calls": calls,
        "outputs": outputs,
        "elapsed_ms": elapsed_ms,
        "items_per_second": if elapsed_ms == 0 { 0.0 } else { outputs as f64 * 1000.0 / elapsed_ms as f64 },
        "checksum": checksum,
        "error": error,
    })
}

pub(crate) fn run_long_probe(
    engine: &mut trufflepig::semantic::SemanticInferenceEngine,
) -> serde_json::Value {
    // The old 8192-token shape must be rejected before ONNX inference now that
    // the public per-input bound is 4096. The model reports the exact count.
    let text = "x ".repeat(8189);
    let started = Instant::now();
    match engine.embed_batch_uncached(&[text.as_str()]) {
        Ok(embeddings) => serde_json::json!({
            "name": "8192_tokens_singleton",
            "status": "failed",
            "requested_token_count": 8192,
            "token_count": 8192,
            "actual_call": true,
            "expected_rejection": true,
            "elapsed_ms": started.elapsed().as_millis(),
            "outputs": embeddings.len(),
            "reason": "input unexpectedly accepted above max_input_tokens"
        }),
        Err(error) => {
            let reason = error.to_string();
            let token_limit_rejected = reason.contains("exceeds 4096 model tokens");
            serde_json::json!({
            "name": "8192_tokens_singleton",
            "status": if token_limit_rejected { "passed" } else { "failed" },
            "outcome": if token_limit_rejected { "expected_rejection" } else { "inference_failure" },
            "requested_token_count": 8192,
            "token_count": parse_token_count(&reason).or(Some(8192)),
            "actual_call": !token_limit_rejected,
            "expected_rejection": token_limit_rejected,
            "elapsed_ms": started.elapsed().as_millis(),
            "reason": reason
            })
        }
    }
}

fn parse_token_count(error: &str) -> Option<usize> {
    error
        .split("got ")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .map(|value| value.trim_matches(|character: char| !character.is_ascii_digit()))
        .and_then(|value| value.parse().ok())
}
