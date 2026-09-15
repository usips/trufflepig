use super::*;
use crate::search::{Query, search_prepared, tests::fixture};
use anyhow::bail;

enum FakeScorer {
    Scores(Vec<f32>),
    Failing(&'static str),
}

impl RerankScorer for FakeScorer {
    fn score(
        &self,
        _cache_identity: &std::path::Path,
        _query: &str,
        _documents: &[String],
    ) -> Result<Vec<f32>> {
        match self {
            FakeScorer::Scores(scores) => Ok(scores.clone()),
            FakeScorer::Failing(message) => bail!("{message}"),
        }
    }
}

fn bare_hit(path: &str, kind: &str, start: usize, end: usize) -> Hit {
    Hit {
        handle: String::new(),
        path: path.to_owned(),
        revision: None,
        start,
        end,
        start_line: 1,
        end_line: 1,
        name: path.to_owned(),
        kind: kind.to_owned(),
        container: None,
        provenance: None,
        resolution: None,
        candidates: Vec::new(),
        target: None,
    }
}

#[test]
fn rerank_reverses_only_the_scored_window() {
    let names: Vec<String> = (0..40).map(|index| format!("f{index:02}.md")).collect();
    let files: Vec<(&str, &[u8])> = names
        .iter()
        .map(|name| (name.as_str(), b"marker\n".as_slice()))
        .collect();
    let (_root, cache, store) = fixture(&files);
    let query = Query::parse("marker").unwrap();
    let mut trace = RetrievalTrace::disabled();
    let scorer = FakeScorer::Scores((0..32).map(|index| index as f32).collect());
    let result = search_prepared(
        &store,
        &query,
        cache.path(),
        None,
        Some(&scorer),
        &mut trace,
    )
    .unwrap();
    let paths: Vec<_> = result.hits.iter().map(|hit| hit.path.clone()).collect();
    let expected_head: Vec<_> = (0..32)
        .rev()
        .map(|index| format!("f{index:02}.md"))
        .collect();
    let expected_tail: Vec<_> = (32..40).map(|index| format!("f{index:02}.md")).collect();
    assert_eq!(paths[..32], expected_head[..]);
    assert_eq!(paths[32..40], expected_tail[..]);
    assert_eq!(result.coverage["rerank_status"], "ready");
    assert_eq!(result.coverage["rerank_window"], 32);
}

#[test]
fn rerank_failure_keeps_fused_order() {
    let (_root, cache, store) = fixture(&[("a.md", b"marker\n"), ("b.md", b"marker\n")]);
    let query = Query::parse("marker").unwrap();
    let mut trace = RetrievalTrace::disabled();
    let scorer = FakeScorer::Failing("rerank_worker_unreachable");
    let result = search_prepared(
        &store,
        &query,
        cache.path(),
        None,
        Some(&scorer),
        &mut trace,
    )
    .unwrap();
    let paths: Vec<_> = result.hits.iter().map(|hit| hit.path.clone()).collect();
    assert_eq!(paths, ["a.md", "b.md"]);
    assert_eq!(result.coverage["rerank_status"], "unavailable");
    assert!(
        result.coverage["rerank_reason"]
            .as_str()
            .unwrap()
            .contains("rerank_worker_unreachable")
    );
}

#[test]
fn rerank_score_count_mismatch_keeps_fused_order() {
    let (_root, cache, store) = fixture(&[("a.md", b"marker\n"), ("b.md", b"marker\n")]);
    let query = Query::parse("marker").unwrap();
    let mut trace = RetrievalTrace::disabled();
    let scorer = FakeScorer::Scores(vec![1.0]);
    let result = search_prepared(
        &store,
        &query,
        cache.path(),
        None,
        Some(&scorer),
        &mut trace,
    )
    .unwrap();
    let paths: Vec<_> = result.hits.iter().map(|hit| hit.path.clone()).collect();
    assert_eq!(paths, ["a.md", "b.md"]);
    assert_eq!(result.coverage["rerank_status"], "unavailable");
    assert!(
        result.coverage["rerank_reason"]
            .as_str()
            .unwrap()
            .contains("score count mismatch")
    );
}

#[test]
fn rerank_non_finite_score_sinks_hit_below_scored_ones() {
    let (_root, cache, store) = fixture(&[
        ("a.md", b"marker\n"),
        ("b.md", b"marker\n"),
        ("c.md", b"marker\n"),
    ]);
    let query = Query::parse("marker").unwrap();
    let mut trace = RetrievalTrace::disabled();
    let scorer = FakeScorer::Scores(vec![1.0, f32::NAN, 2.0]);
    let result = search_prepared(
        &store,
        &query,
        cache.path(),
        None,
        Some(&scorer),
        &mut trace,
    )
    .unwrap();
    let paths: Vec<_> = result.hits.iter().map(|hit| hit.path.clone()).collect();
    assert_eq!(paths, ["c.md", "a.md", "b.md"]);
    assert_eq!(result.coverage["rerank_status"], "ready");
    assert_eq!(result.coverage["rerank_window"], 2);
}

#[test]
fn rerank_disabled_sets_no_coverage_keys() {
    let (_root, cache, store) = fixture(&[("a.md", b"marker\n")]);
    let query = Query::parse("marker").unwrap();
    let mut trace = RetrievalTrace::disabled();
    let result = search_prepared(&store, &query, cache.path(), None, None, &mut trace).unwrap();
    assert!(result.coverage.get("rerank_status").is_none());
    assert!(result.coverage.get("rerank_reason").is_none());
    assert!(result.coverage.get("rerank_window").is_none());
}

#[test]
fn clamp_document_stops_on_a_char_boundary() {
    let mut body = "a".repeat(4095);
    body.push('é'); // two-byte character straddling the 4096-byte clamp
    body.push_str("tail");
    let clamped = clamp_document(body.as_bytes()).unwrap();
    assert_eq!(clamped.len(), 4095);
    assert!(std::str::from_utf8(clamped.as_bytes()).is_ok());
}

#[test]
fn clamp_document_treats_a_non_utf8_head_as_unscored() {
    assert!(clamp_document(&[0xff, 0xfe, 0xfd]).is_none());
    assert_eq!(clamp_document(b"ok\xffrest").unwrap(), "ok");
}

#[test]
fn rerank_scores_a_filename_hit_from_the_file_head() {
    let (_root, cache, store) = fixture(&[("head.md", b"marker at the very start\n")]);
    let mut trace = RetrievalTrace::disabled();
    let mut coverage = serde_json::json!({});
    let mut hits = vec![bare_hit("head.md", "file", 0, 0)];
    let scorer = FakeScorer::Scores(vec![1.0]);
    apply(
        &store,
        "marker",
        cache.path(),
        &scorer,
        &mut hits,
        &mut coverage,
        &mut trace,
    );
    assert_eq!(coverage["rerank_status"], "ready");
    assert_eq!(coverage["rerank_window"], 1);
}

#[test]
fn rerank_leaves_a_hit_without_a_content_row_unscored_below_scored_hits() {
    let (_root, cache, store) = fixture(&[("a.md", b"marker\n"), ("excluded.bin", b"\0binary")]);
    let mut trace = RetrievalTrace::disabled();
    let mut coverage = serde_json::json!({});
    let mut hits = vec![
        bare_hit("excluded.bin", "file", 0, 0),
        bare_hit("a.md", "file", 0, 0),
    ];
    let scorer = FakeScorer::Scores(vec![1.0]);
    apply(
        &store,
        "marker",
        cache.path(),
        &scorer,
        &mut hits,
        &mut coverage,
        &mut trace,
    );
    let paths: Vec<_> = hits.iter().map(|hit| hit.path.clone()).collect();
    assert_eq!(paths, ["a.md", "excluded.bin"]);
    assert_eq!(coverage["rerank_status"], "ready");
    assert_eq!(coverage["rerank_window"], 1);
}
