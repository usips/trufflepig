use super::*;

#[test]
fn shared_sections_follow_rendered_heading_titles_and_task_coverage() {
    let (_directory, mut board) = database();
    let body = "# **Build** `API` &amp; [deploy](https://example.test) ###\n\n\
                Setext *section*\n----------------\n\n\
                ## Setext section\n\n```\n# fenced\n```\n";
    let BoardResult::Change(created) = call(
        &mut board,
        "human",
        BoardOp::New {
            title: PlanTitle::new("Heading coverage").unwrap(),
            body: PlanText::new(body).unwrap(),
            steward: None,
            repo_key: None,
        },
    ) else {
        panic!("expected plan");
    };
    let plan = created.plan.unwrap();
    call(
        &mut board,
        "human",
        BoardOp::TaskCreate {
            plan,
            title: PlanTitle::new("Build work").unwrap(),
            to: None,
            section: Some("Build API & deploy".into()),
        },
    );
    let BoardResult::Plan(view) = call(
        &mut board,
        "codex",
        BoardOp::Show {
            target: BoardRef::Plan(plan),
        },
    ) else {
        panic!("expected plan view");
    };
    let headings = crate::board::board_markup::headings(body);
    assert_eq!(
        headings
            .iter()
            .map(|heading| heading.title.as_str())
            .collect::<Vec<_>>(),
        ["Build API & deploy", "Setext section", "Setext section"]
    );
    assert_eq!(view.sections_without_tasks, ["Setext section"]);
}
