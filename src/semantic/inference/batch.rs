use super::{MAX_BATCH_INPUTS, MAX_BATCH_PADDED_TOKENS, MAX_INPUT_TOKENS};
use crate::semantic::Embedding;
use anyhow::{Context, Result, bail};
use fastembed::TextEmbedding;
use std::ops::Range;

fn token_length(model: &TextEmbedding, text: &str, index: usize) -> Result<usize> {
    let encoded = model
        .tokenizer
        .encode(text, true)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let length = encoded.len();
    if length > MAX_INPUT_TOKENS {
        bail!(
            "semantic_unavailable: input at batch index {index} exceeds {MAX_INPUT_TOKENS} model tokens (got {length})"
        );
    }
    Ok(length)
}

pub(super) fn plan_batch_ranges(token_lengths: &[usize]) -> Result<Vec<Range<usize>>> {
    for (index, &length) in token_lengths.iter().enumerate() {
        if length > MAX_INPUT_TOKENS {
            bail!(
                "semantic_unavailable: input at batch index {index} exceeds {MAX_INPUT_TOKENS} model tokens (got {length})"
            );
        }
    }
    let mut ranges = Vec::with_capacity(token_lengths.len().div_ceil(MAX_BATCH_INPUTS));
    let mut start = 0;
    while start < token_lengths.len() {
        let mut end = start;
        let mut padded_tokens = 0;
        while end < token_lengths.len() && end - start < MAX_BATCH_INPUTS {
            let candidate_max = padded_tokens.max(token_lengths[end]);
            let candidate_count = end - start + 1;
            if candidate_max.saturating_mul(candidate_count) > MAX_BATCH_PADDED_TOKENS {
                break;
            }
            padded_tokens = candidate_max;
            end += 1;
        }
        if end == start {
            bail!(
                "semantic_unavailable: input at batch index {start} cannot fit the padded token limit"
            );
        }
        ranges.push(start..end);
        start = end;
    }
    Ok(ranges)
}

pub(super) fn infer_batch(model: &mut TextEmbedding, texts: &[&str]) -> Result<Vec<Embedding>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let token_lengths = texts
        .iter()
        .enumerate()
        .map(|(index, text)| token_length(model, text, index))
        .collect::<Result<Vec<_>>>()?;
    let ranges = plan_batch_ranges(&token_lengths)?;
    let mut embeddings = Vec::with_capacity(texts.len());
    for range in ranges {
        let batch = model
            .embed(&texts[range.clone()], Some(range.len()))
            .with_context(|| {
                format!(
                    "semantic_unavailable: inference failed for batch inputs {}..{}",
                    range.start, range.end
                )
            })?;
        if batch.len() != range.len() {
            bail!(
                "semantic_unavailable: model returned {} embeddings for {} inputs",
                batch.len(),
                range.len()
            );
        }
        for (offset, vector) in batch.iter().enumerate() {
            embeddings.push(Embedding::from_values(vector).with_context(|| {
                format!(
                    "semantic_unavailable: invalid model output for batch input {}",
                    range.start + offset
                )
            })?);
        }
    }
    Ok(embeddings)
}

pub(super) fn infer_batch_isolated(
    model: &mut TextEmbedding,
    texts: &[&str],
) -> Vec<Result<Embedding>> {
    let mut results = (0..texts.len())
        .map(|_| None)
        .collect::<Vec<Option<Result<Embedding>>>>();
    let mut valid = Vec::with_capacity(texts.len());
    for (index, text) in texts.iter().enumerate() {
        match token_length(model, text, index) {
            Ok(length) => valid.push((index, length)),
            Err(error) => results[index] = Some(Err(error)),
        }
    }

    let lengths = valid.iter().map(|(_, length)| *length).collect::<Vec<_>>();
    let ranges = match plan_batch_ranges(&lengths) {
        Ok(ranges) => ranges,
        Err(error) => {
            let error = error.to_string();
            for (index, _) in valid {
                results[index] = Some(Err(anyhow::anyhow!(error.clone())));
            }
            return results
                .into_iter()
                .map(|result| result.expect("every batch input has a result"))
                .collect();
        }
    };

    for range in ranges {
        isolate_batch(model, texts, &valid, range, &mut results);
    }
    results
        .into_iter()
        .map(|result| result.expect("every batch input has a result"))
        .collect()
}

fn isolate_batch(
    model: &mut TextEmbedding,
    texts: &[&str],
    valid: &[(usize, usize)],
    range: Range<usize>,
    results: &mut [Option<Result<Embedding>>],
) {
    let original_indices = valid[range.clone()]
        .iter()
        .map(|(index, _)| *index)
        .collect::<Vec<_>>();
    let batch = original_indices
        .iter()
        .map(|index| texts[*index])
        .collect::<Vec<_>>();
    match infer_one_planned_batch(model, &batch, &original_indices) {
        Ok(embeddings) => {
            for (index, embedding) in original_indices.into_iter().zip(embeddings) {
                results[index] = Some(Ok(embedding));
            }
        }
        Err(error) if range.len() == 1 => {
            results[original_indices[0]] = Some(Err(error));
        }
        Err(_) => {
            let middle = range.start + range.len() / 2;
            isolate_batch(model, texts, valid, range.start..middle, results);
            isolate_batch(model, texts, valid, middle..range.end, results);
        }
    }
}

fn infer_one_planned_batch(
    model: &mut TextEmbedding,
    texts: &[&str],
    original_indices: &[usize],
) -> Result<Vec<Embedding>> {
    let batch = model.embed(texts, Some(texts.len())).with_context(|| {
        format!(
            "semantic_unavailable: inference failed for batch inputs {}..{}",
            original_indices.first().copied().unwrap_or(0),
            original_indices
                .last()
                .copied()
                .unwrap_or(0)
                .saturating_add(1)
        )
    })?;
    if batch.len() != texts.len() {
        bail!(
            "semantic_unavailable: model returned {} embeddings for {} inputs",
            batch.len(),
            texts.len()
        );
    }
    batch
        .iter()
        .enumerate()
        .map(|(offset, vector)| {
            Embedding::from_values(vector).with_context(|| {
                format!(
                    "semantic_unavailable: invalid model output for batch input {}",
                    original_indices[offset]
                )
            })
        })
        .collect()
}

pub(super) fn infer(model: &mut TextEmbedding, text: &str) -> Result<Embedding> {
    infer_batch(model, &[text])?
        .pop()
        .context("semantic_unavailable: missing model output")
}
