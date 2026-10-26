use super::*;
use crate::results::ResultSet;
use serde_json::json;

#[test]
fn live_regex_read_and_walk_failures_remain_partial_with_a_hint() -> Result<()> {
    let query = Query::parse("re:absent_marker")?;
    for (read_failures, walk_failures) in [(1, 0), (0, 1)] {
        let mut set = ResultSet {
            generation: 0,
            coverage: json!({
                "live_checked_files": 2,
                "live_excluded_files": 3,
                "live_read_failures": read_failures,
                "live_walk_failures": walk_failures,
            }),
            truncated: false,
            hits: Vec::new(),
        };
        let current = live_coverage(&set, &query);
        assert!(!current.complete);
        assert_eq!(current.limit, Some(LIMIT_SOURCE_FAILURES));
        assert_eq!(current.unsearched_unknown, walk_failures > 0);

        coverage::apply(&mut set, &current, FollowUp::Search(&query), false, false);
        assert_eq!(set.coverage["complete"], false);
        assert_eq!(set.coverage["absence"], "not_proven");
        assert_eq!(set.coverage["current_source_unsearched"], 1);
        assert_eq!(set.coverage["current_source"]["excluded_files"], 3);
        assert_eq!(
            set.coverage["current_source"]["unsearched_unknown"],
            walk_failures > 0
        );
        assert!(
            set.coverage["hint"]
                .as_str()
                .unwrap()
                .contains("1 source reads or walk entries failed")
        );
        assert!(set.truncated);
    }
    Ok(())
}
