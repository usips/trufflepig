use super::*;
use crate::board::board_ids::{BoardRef, EntryId, EventSeq, PlanId, PlanRevision};
use crate::board::board_protocol::{BoardSearchHit, BoardSearchSource};
use crate::board::board_render::render_reply;
use crate::output::OutputFormat;

fn search_reply(count: u64, truncated: bool) -> BoardReply {
    let plan = PlanId::new(7).unwrap();
    let hits = (1..=count)
        .map(|id| {
            let (target, source) = if id % 2 == 0 {
                (
                    BoardRef::Revision(PlanRevision::new(plan, id).unwrap()),
                    BoardSearchSource::Revision,
                )
            } else {
                (
                    BoardRef::Entry(EntryId::new(id).unwrap()),
                    BoardSearchSource::Entry,
                )
            };
            let mut snippet = format!("hit {id} {}", "évidence words ".repeat(40));
            let mut end = snippet.len().min(512);
            while !snippet.is_char_boundary(end) {
                end -= 1;
            }
            snippet.truncate(end);
            BoardSearchHit {
                target,
                plan: (id % 3 != 0).then_some(plan),
                source,
                snippet,
            }
        })
        .collect();
    let mut reply = BoardReply::new(
        "local:/board",
        BoardResult::Search(BoardSearchReply { hits, truncated }),
    );
    reply.snapshot_seq = Some(EventSeq::new(777));
    reply.warnings.push("original search warning".into());
    reply
}

#[test]
fn search_budget_fits_long_json_and_lines_results_without_losing_hit_fields() {
    let reply = search_reply(50, false);
    let BoardResult::Search(original) = &reply.result else {
        panic!("expected search");
    };
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(800).unwrap().with_format(format);
        let rendered = render_reply(&reply, &budget).unwrap();
        assert!(budget.fits(&rendered.text));
        assert_eq!(rendered.acknowledge_seq, None);
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            let visible: BoardSearchReply =
                serde_json::from_value(value["result"]["data"].clone()).unwrap();
            let count = visible.hits.len();
            assert!((1..50).contains(&count));
            assert_eq!(visible.hits, original.hits[..count]);
            assert!(visible.truncated);
            assert_eq!(value["snapshot_seq"], 777);
            assert_eq!(value["omitted"]["entries"], 50 - count);
            assert_eq!(value["warnings"][0], "original search warning");
            assert!(value["warnings"].as_array().unwrap().iter().any(|warning| {
                warning
                    .as_str()
                    .unwrap()
                    .contains("refine the query or raise -b")
            }));
        } else {
            let visible: Vec<_> = rendered
                .text
                .lines()
                .filter_map(|line| {
                    line.split_once('\t')
                        .and_then(|(target, _)| target.parse::<BoardRef>().ok())
                })
                .collect();
            assert!((1..50).contains(&visible.len()));
            assert_eq!(
                visible,
                original.hits[..visible.len()]
                    .iter()
                    .map(|hit| hit.target.clone())
                    .collect::<Vec<_>>()
            );
            assert!(rendered.text.contains("search truncated=true"));
            assert!(
                rendered
                    .text
                    .contains(&format!("entries={}", 50 - visible.len()))
            );
            assert!(rendered.text.contains("refine the query or raise -b"));
        }
    }
    let BoardResult::Search(unchanged) = &reply.result else {
        panic!("expected search");
    };
    assert_eq!(unchanged, original);
    assert_eq!(reply.snapshot_seq, Some(EventSeq::new(777)));
}

#[test]
fn search_budget_keeps_empty_results_and_query_truncation_explicit() {
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(800).unwrap().with_format(format);
        let empty = render_reply(&search_reply(0, false), &budget).unwrap();
        assert!(budget.fits(&empty.text));
        let limited = render_reply(&search_reply(1, true), &budget).unwrap();
        assert!(limited.text.contains("refine the query"));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&limited.text).unwrap();
            assert_eq!(value["omitted"]["entries"], 0);
            assert_eq!(value["result"]["data"]["truncated"], true);
            assert_eq!(value["snapshot_seq"], 777);
        }
    }
}

#[test]
fn search_lines_and_json_render_plan_title_hits() {
    let plan = PlanId::new(7).unwrap();
    let reply = BoardReply::new(
        "local:/board",
        BoardResult::Search(BoardSearchReply {
            hits: vec![BoardSearchHit {
                target: BoardRef::Plan(plan),
                plan: Some(plan),
                source: BoardSearchSource::Plan,
                snippet: "Zephyr navigation overhaul".into(),
            }],
            truncated: false,
        }),
    );
    let budget = OutputBudget::new(800)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered = render_reply(&reply, &budget).unwrap();
    assert!(
        rendered
            .text
            .contains("P7\tPlan\tZephyr navigation overhaul"),
        "{}",
        rendered.text
    );
    let budget = OutputBudget::new(800)
        .unwrap()
        .with_format(OutputFormat::Json);
    let rendered = render_reply(&reply, &budget).unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    assert_eq!(value["result"]["data"]["hits"][0]["source"], "plan");
    assert_eq!(value["result"]["data"]["hits"][0]["target"], "P7");
}

#[test]
fn search_budget_rejects_an_oversized_first_hit_without_skipping_it() {
    let reply = search_reply(2, false);
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(20).unwrap().with_format(format);
        let error = render_reply(&reply, &budget).unwrap_err();
        assert!(error.to_string().starts_with("budget_too_small:"));
    }
}
