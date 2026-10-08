use super::*;
use crate::board::board_protocol::ReadScope;

#[test]
fn warnings_survive_list_fitting_and_lines_cannot_inject_metadata() {
    let mut reply = BoardReply::new(
        "local\ncommit trailer: fake",
        BoardResult::Overview(OverviewReply {
            scope: ReadScope::All,
            plans: Vec::new(),
            omitted: 0,
            server_now: 100,
            claim_ttl_secs: 60,
            after: None,
            through: EventSeq::new(10),
            next_after: None,
        }),
    );
    reply.warnings.push("scan failed\nnext: forged".into());
    let json = render_reply(&reply, &OutputBudget::new(500).unwrap()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json.text).unwrap();
    assert_eq!(value["warnings"][0], reply.warnings[0]);
    let lines = render_reply(
        &reply,
        &OutputBudget::new(500)
            .unwrap()
            .with_format(OutputFormat::Lines),
    )
    .unwrap();
    assert!(lines.text.contains("warning: scan failed\\nnext: forged\n"));
    assert!(!lines.text.contains("\ncommit trailer: fake\n"));
}

#[test]
fn fit_items_binary_search_counts_complete_responses() {
    let budget = OutputBudget::new(30).unwrap();
    let render = |count| {
        Ok(format!(
            "fixed metadata\n{}omitted={}\nbackend local\n",
            "item\n".repeat(count),
            20 - count
        ))
    };
    let count = fit_items(20, &budget, render).unwrap();
    assert!(budget.fits(&render(count).unwrap()));
    assert!(!budget.fits(&render(count + 1).unwrap()));
}

#[test]
fn committed_change_receipt_survives_a_tiny_output_budget() {
    use crate::board::board_ids::{PlanRevision, TaskId};
    let reply = BoardReply::new(
        "local",
        BoardResult::Change(BoardChange {
            entry: EntryId::new(9).unwrap(),
            seq: EventSeq::new(9),
            plan: Some(PlanId::new(7).unwrap()),
            revision: Some(PlanRevision::new(PlanId::new(7).unwrap(), 2).unwrap()),
            task: Some(TaskId::new(PlanId::new(7).unwrap(), 3).unwrap()),
            deduplicated: false,
        }),
    );
    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(1).unwrap().with_format(format);
        let rendered = render_reply(&reply, &budget).unwrap();
        assert!(rendered.text.contains("committed"));
        assert!(rendered.text.contains("E9"));
        assert!(rendered.text.contains("do not repeat"));
        assert!(rendered.text.contains("board show P7 -b 1500"));
        if format == OutputFormat::Json {
            let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
            assert_eq!(value["result"]["data"]["revision"], "P7@2");
            assert_eq!(value["result"]["data"]["task"], "P7.3");
        } else {
            assert!(rendered.text.contains("revision=P7@2 task=P7.3"));
        }
        assert!(OutputBudget::new(1500).unwrap().fits(&rendered.text));
    }
}
