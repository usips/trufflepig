use super::*;

#[test]
fn ssot_diff_retains_original_newline_and_crlf_bytes() {
    let diff = build_ssot_diff(&revision(1, "same\r\nold\r\n"), &revision(2, "same\r\nnew"));
    assert_eq!(diff.hunks.len(), 1);
    assert_eq!(diff.hunks[0].removed, ["old\r\n"]);
    assert_eq!(diff.hunks[0].added, ["new"]);
    assert_eq!(diff.hunks[0].context_before, ["same\r\n"]);
}

#[test]
fn adjacent_diff_context_contains_only_unchanged_lines() {
    let diff = build_ssot_diff(
        &revision(1, "start\nold a\nmiddle\nold b\nend\n"),
        &revision(2, "start\nnew a\nmiddle\nnew b\nend\n"),
    );
    assert_eq!(diff.hunks.len(), 2);
    for hunk in &diff.hunks {
        for line in hunk.context_before.iter().chain(&hunk.context_after) {
            assert!(matches!(line.as_str(), "start\n" | "middle\n" | "end\n"));
        }
    }
}

#[test]
fn insertion_diff_header_counts_the_context_it_displays() {
    let reply = crate::board::board_protocol::BoardReply::new(
        "local",
        crate::board::board_protocol::BoardResult::Diff(
            crate::board::board_protocol::RevisionDiff {
                before: revision(1, "A\nB\n"),
                after: revision(2, "A\nX\nB\n"),
            },
        ),
    );
    let budget = OutputBudget::new(500)
        .unwrap()
        .with_format(OutputFormat::Lines);
    let rendered = crate::board::board_render::render_reply(&reply, &budget).unwrap();
    assert!(rendered.text.contains("@@ -1,2 +1,3 @@\n A\n+X\n B\n"));
}

#[test]
fn review_trimming_reports_each_omission_and_keeps_drill_and_diff_hint() {
    if crate::board::board_test_support::git_version() < Some((2, 55)) {
        eprintln!(concat!(
            "skipping review_trimming_reports_each_omission_and_keeps_drill_and_diff_hint: ",
            "requires Git >= 2.55 for history drill hints"
        ));
        return;
    }
    let mut source = evidence();
    for seq in 10..20 {
        source.entries.push(EntryRecord {
            via: None,
            id: EntryId::new(seq).unwrap(),
            plan: Some(source.plan.id),
            kind: EntryKind::Progress,
            body: EntryText::new("entry with individually useful facts ".repeat(90)).unwrap(),
            to: None,
            supersedes: None,
            actor: actor("codex"),
            model: None,
            effort: None,
            repo_key: None,
            state: None,
            refs: Vec::new(),
            seq: EventSeq::new(seq),
            created_at: 120,
        });
    }
    source.base = revision(
        1,
        &format!("context\n{}", "old text for line\n".repeat(1000)),
    );
    source.head = revision(
        2,
        &format!("context\n{}", "new text for line\n".repeat(1000)),
    );
    let fixture = crate::board::repo_identity::tests::GitFixture::new();
    let oid = fixture.commit("root");
    let quoted = fixture.directory.path().join("it's here");
    fixture.git(&[
        "clone",
        "--quiet",
        fixture.root.to_str().unwrap(),
        quoted.to_str().unwrap(),
    ]);
    let mut linked = commit("codex", 120);
    linked.oid = oid;
    source.commits.push(linked.clone());
    let repositories = vec![RepoScanTarget {
        registration: crate::board::board_protocol::RepoRegistration {
            root_commits: Vec::new(),
            registration_error: None,
            origin_override: None,
            repo_key: linked.repo_key.clone(),
            origin_label: None,
            host: "laptop".into(),
            common_dir: quoted.join(".git"),
            plan_id: Some(source.plan.id),
        },
        oldest_plan_at: 100,
        plans: vec![source.plan.id],
        scan_error: None,
    }];
    let packet = assemble_review(
        &source,
        source.agent.as_ref(),
        &repositories,
        Vec::new(),
        Vec::new(),
    );
    assert!(packet.linked[0].drill.as_ref().unwrap().contains("'\"'\"'"));
    let budget = OutputBudget::new(900).unwrap();
    let rendered = render_review(&packet, &budget, "local").unwrap();
    let value: serde_json::Value = serde_json::from_str(&rendered.text).unwrap();
    let data = &value["result"]["data"];
    assert_eq!(
        data["entries"].as_array().unwrap().len() as u64
            + data["omitted"]["entries"].as_u64().unwrap(),
        10
    );
    assert!(data["omitted"]["diff_context_lines"].as_u64().unwrap() > 0);
    assert_eq!(data["omitted"]["diff_body_lines"], 2000);
    assert_eq!(data["ssot_diff"]["next"], "board show P7@1..2");
    assert!(
        data["linked"][0]["drill"]
            .as_str()
            .unwrap()
            .contains("trufflepig-agent --root")
    );
    assert!(budget.fits(&rendered.text));
    let lines = render_review(
        &packet,
        &OutputBudget::new(900)
            .unwrap()
            .with_format(OutputFormat::Lines),
        "local",
    )
    .unwrap();
    let shown_entries = lines
        .text
        .lines()
        .filter(|line| line.starts_with('E') && line.contains("\tprogress\t"))
        .count();
    assert!(
        lines
            .text
            .contains(&format!("entries={}", 10 - shown_entries)),
        "{}",
        lines.text
    );
    assert!(lines.text.contains("next: board show P7@1..2"));
    assert!(lines.text.contains("commit trailer: Plan: P7"));
    assert!(
        lines
            .text
            .contains("coauthor: codex (claimed model) <agent@example.com>")
    );
    assert!(lines.text.contains("Plan: P7 Plan-Task: P7.1"));
}
