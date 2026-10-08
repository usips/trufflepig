use super::*;
use crate::board::board_protocol::AgentClaims;

#[test]
fn entry_and_event_vendor_use_stored_model_snapshots() {
    let (_directory, mut board) = database();
    let plan = plan(&mut board);
    let mut posted = Vec::with_capacity(11);
    for (index, (harness, model, vendor)) in [
        ("muse", Some("Claude Sonnet 4.5"), "claude"),
        ("omp", Some("gpt-6.1-sol"), "codex"),
        ("omp", Some("Kimi K2"), "kimi"),
        ("omp", Some("Grok 4"), "grok"),
        ("omp", Some("Gemini 3 Pro"), "gemini"),
        ("omp", Some("Qwen3"), "qwen"),
        ("omp", Some("Muse Spark"), "muse"),
        ("omp", Some("Llama 4 Maverick"), "muse"),
        ("human", Some("gpt-6.1-sol"), "human"),
        ("cli", Some("Claude Sonnet 4.5"), "human"),
        ("omp", None, "unknown"),
    ]
    .into_iter()
    .enumerate()
    {
        let actor = BoardActor::new(
            "josh",
            "laptop",
            HarnessLabel::parse(harness).unwrap(),
            format!("vendor-{index}"),
        )
        .unwrap();
        let mut request = BoardRequest::new(
            actor.clone(),
            BoardOp::Post {
                target: BoardRef::Plan(plan),
                kind: EntryKind::Note,
                body: EntryText::new(format!("vendor snapshot {index}")).unwrap(),
                to: None,
                supersedes: None,
            },
        );
        request.claims = Some(AgentClaims {
            model: model.map(str::to_owned),
            effort: None,
        });
        let BoardResult::Change(change) = board.handle(&request).unwrap().result else {
            panic!("expected post receipt");
        };
        board
            .handle(&BoardRequest::new(
                actor,
                BoardOp::Hello {
                    model: "Grok replacement model".into(),
                    effort: None,
                },
            ))
            .unwrap();
        posted.push((change, model, vendor));
    }
    let (_, events) = board
        .read_event_batch(EventSeq::new(0), Some(plan), 100)
        .unwrap();
    for (change, model, expected) in posted {
        let reply = call(
            &mut board,
            "codex",
            BoardOp::Show {
                target: BoardRef::Entry(change.entry),
            },
        );
        let BoardResult::Entry(view) = reply else {
            panic!("expected entry view");
        };
        assert_eq!(view.entry.model.as_deref(), model);
        assert_eq!(
            serde_json::to_value(&view.entry).unwrap()["vendor"],
            expected
        );
        let event = events.iter().find(|event| event.seq == change.seq).unwrap();
        assert_eq!(event.model.as_deref(), model);
        assert_eq!(serde_json::to_value(event).unwrap()["vendor"], expected);
    }
}
