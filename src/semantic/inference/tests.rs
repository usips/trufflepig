use super::batch::plan_batch_ranges;
use super::*;

#[test]
fn provider_defaults_are_safe_for_accuracy_and_memory() {
    let cpu = SemanticProviderConfig::default();
    assert_eq!(cpu.provider, SemanticProvider::Cpu);
    assert_eq!(cpu.arena_max_bytes, DEFAULT_CUDA_ARENA_MAX_BYTES);

    let cuda = SemanticProviderConfig::cuda(3);
    assert_eq!(cuda.provider, SemanticProvider::Cuda { device_ordinal: 3 });
    assert_eq!(cuda.arena_max_bytes, DEFAULT_CUDA_ARENA_MAX_BYTES);
}

#[test]
fn provider_validation_rejects_invalid_values_without_loading_model() {
    assert!(SemanticProviderConfig::cuda(-1).validate().is_err());
    assert!(
        SemanticProviderConfig::cuda(0)
            .with_arena_max_bytes(0)
            .validate()
            .is_err()
    );
}

#[test]
fn batch_planner_respects_input_and_padded_token_limits() {
    let lengths = [1000; 17];
    let ranges = plan_batch_ranges(&lengths).unwrap();
    assert_eq!(ranges, [0..8, 8..16, 16..17]);
    for range in ranges {
        let max_tokens = lengths[range.clone()].iter().copied().max().unwrap();
        assert!(range.len() <= MAX_BATCH_INPUTS);
        assert!(max_tokens * range.len() <= MAX_BATCH_PADDED_TOKENS);
    }
}

#[test]
fn batch_planner_splits_at_padded_token_boundary() {
    let ranges = plan_batch_ranges(&[4096, 4096, 4096, 1]).unwrap();
    assert_eq!(ranges, [0..2, 2..4]);
}

#[test]
fn batch_planner_rejects_an_oversized_input() {
    let error = plan_batch_ranges(&[MAX_INPUT_TOKENS + 1]).unwrap_err();
    assert!(error.to_string().contains("exceeds 4096 model tokens"));
}
