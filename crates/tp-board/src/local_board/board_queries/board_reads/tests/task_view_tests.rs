use super::*;
use crate::board_protocol::ClaimResume;

fn create_task(board: &mut LocalBoard, plan: PlanId, title: &str) -> TaskId {
    let BoardResult::Change(change) = call(
        board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new(title).unwrap(),
            to: None,
            section: None,
        },
    ) else {
        panic!("expected task receipt");
    };
    change
        .task
        .expect("task creation includes its task reference")
}

fn post_task(
    board: &mut LocalBoard,
    harness: &str,
    task: TaskId,
    kind: EntryKind,
    body: &str,
) -> EntryId {
    let BoardResult::Change(change) = call(
        board,
        harness,
        BoardOp::Post {
            target: BoardRef::Task(task),
            kind,
            body: EntryText::new(body).unwrap(),
            to: None,
            supersedes: None,
            progress: None,
            verdict: None,
        },
    ) else {
        panic!("expected task entry receipt");
    };
    change.entry
}

fn show_task(board: &mut LocalBoard, task: TaskId) -> crate::board_protocol::TaskView {
    let BoardResult::TaskView(view) = call(
        board,
        "codex",
        BoardOp::Show {
            target: BoardRef::Task(task),
        },
    ) else {
        panic!("expected task detail");
    };
    view
}

#[test]
fn task_view_uses_persisted_task_refs_and_excludes_neighbor_entries() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let task = create_task(&mut board, plan, "First task");
    let neighbor = create_task(&mut board, plan, "Neighbor task");
    let question = post_task(
        &mut board,
        "claude",
        task,
        EntryKind::Question,
        "This prose does not name the task id.",
    );
    post_task(
        &mut board,
        "muse",
        neighbor,
        EntryKind::Note,
        "Neighbor-only evidence",
    );
    post(
        &mut board,
        "muse",
        plan,
        EntryKind::Note,
        "A plan-level note is not task evidence.",
    );

    let view = show_task(&mut board, task);
    assert_eq!(view.task.id, task);
    assert_eq!(view.open_questions, 1);
    assert_eq!(view.oldest_open_question, Some(question));
    assert_eq!(view.entries.len(), 2);
    let question_entry = view
        .entries
        .iter()
        .find(|entry| entry.id == question)
        .unwrap();
    assert!(question_entry.refs.contains(&BoardRef::Task(task)));
    assert!(
        !view
            .entries
            .iter()
            .any(|entry| entry.body.to_string().contains("Neighbor-only evidence"))
    );
    assert!(
        !view
            .entries
            .iter()
            .any(|entry| entry.body.to_string().contains("plan-level note"))
    );
    assert_eq!(view.entries_omitted, 0);
    assert!(view.commits.is_empty());
}

#[test]
fn task_view_separates_the_current_holder_from_claim_history() {
    let (_directory, mut board) = database();
    let plan = new_plan(&mut board);
    let task = create_task(&mut board, plan, "Takeover history");
    for (harness, scope) in [("codex", "first holder"), ("muse", "current holder")] {
        call(
            &mut board,
            harness,
            BoardOp::ClaimTask {
                task,
                scope: Some(EntryText::new(scope).unwrap()),
                resume: ClaimResume::No,
                delegate: None,
            },
        );
        if harness == "codex" {
            call(
                &mut board,
                harness,
                BoardOp::TaskMove {
                    task,
                    column: TaskColumn::Todo,
                    to: None,
                },
            );
        }
    }

    let view = show_task(&mut board, task);
    assert_eq!(
        view.current_claim.as_ref().unwrap().actor.harness.as_str(),
        "muse"
    );
    assert_eq!(view.claim_history.len(), 1);
    assert_eq!(view.claim_history[0].actor.harness.as_str(), "codex");
    assert!(view.claim_history[0].ended_at.is_some());
    assert_eq!(view.claims_omitted, 0);
}
