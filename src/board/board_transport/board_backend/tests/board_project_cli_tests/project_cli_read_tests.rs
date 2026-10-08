use super::*;
use std::collections::BTreeSet;

#[test]
fn project_ids_and_reserved_unscoped_keep_distinct_scopes() {
    isolated(
        "project_ids_and_reserved_unscoped_keep_distinct_scopes",
        |root| {
            let fixture = ProjectCliFixture::new(root, ["unscoped", "W2"]);
            fixture.seed();
            let reads = [
                (
                    vec!["board", "show", "--project", &fixture.project_ids[0]],
                    vec!["P1", "P2"],
                ),
                (vec!["board", "show", "--project", "unscoped"], vec!["P4"]),
                (vec!["board", "show"], vec!["P1", "P4"]),
                (vec!["board", "show", "--all"], vec!["P1", "P2", "P3", "P4"]),
            ];
            for (words, expected) in reads {
                let output = fixture.run(&fixture.repos[0].root, &words).unwrap();
                assert_eq!(overview_plan_ids(&output), expected, "{words:?}");
            }
            let output = fixture.run(&fixture.root, &["board", "show"]).unwrap();
            assert_eq!(overview_plan_ids(&output), ["P1", "P2", "P3", "P4"]);
        },
    );
}

#[test]
fn project_reads_scope_inbox_attention_and_their_continuations() {
    isolated(
        "project_reads_scope_inbox_attention_and_their_continuations",
        |root| {
            let fixture = ProjectCliFixture::new(root, ["W1", "W2"]);
            fixture.seed_questions();
            for (words, collection) in [
                (vec!["board", "inbox", "0", "--project", "W1"], "events"),
                (
                    vec!["board", "attention", "--project", &fixture.project_ids[0]],
                    "entries",
                ),
            ] {
                let output = fixture.run(&fixture.repos[2].root, &words).unwrap();
                let output: Value = serde_json::from_str(&output).unwrap();
                let plans: BTreeSet<_> = output["result"]["data"][collection]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|record| record["plan"].as_str())
                    .collect();
                assert_eq!(plans, BTreeSet::from(["P1", "P2"]), "{words:?}");
                if collection == "events" {
                    assert!(output["next"].as_str().unwrap().contains("--project W1"));
                }
            }
            for command in ["show", "attention"] {
                let output = fixture
                    .run(
                        &fixture.repos[2].root,
                        &["board", command, "--project", "W1", "-n1"],
                    )
                    .unwrap();
                let output: Value = serde_json::from_str(&output).unwrap();
                let next = output["next"].as_str().unwrap();
                assert!(next.contains("--project W1"), "{next}");
                let words: Vec<_> = next.split_whitespace().collect();
                let continuation = fixture.run(&fixture.repos[2].root, &words).unwrap();
                let continuation: Value = serde_json::from_str(&continuation).unwrap();
                let scope = &continuation["result"]["data"]["scope"];
                assert_eq!(scope["keys"].as_array().unwrap().len(), 2);
            }
        },
    );
}

#[test]
fn board_projects_reports_counts_and_stale_registry_entries() {
    isolated(
        "board_projects_reports_counts_and_stale_registry_entries",
        |root| {
            let fixture = ProjectCliFixture::new(root, ["W1", "W2"]);
            fixture.seed();
            let registry = root.join("config/trufflepig/workspaces.toml");
            let text = std::fs::read_to_string(&registry).unwrap();
            std::fs::write(
                registry,
                text.replace(
                    "]\n",
                    &format!(", {:?}]\n", root.join("deleted-workspace.toml")),
                ),
            )
            .unwrap();
            let output = fixture
                .run(&fixture.repos[0].root, &["board", "projects"])
                .unwrap();
            let output: Value = serde_json::from_str(&output).unwrap();
            let projects = output["result"]["data"].as_array().unwrap();
            assert_eq!(projects.len(), 3);
            for (name, count) in [("W1", 2), ("W2", 1)] {
                let project = projects
                    .iter()
                    .find(|project| project["name"] == name)
                    .unwrap();
                assert_eq!(project["plan_count"], count);
                assert!(project["unavailable"].as_array().unwrap().is_empty());
            }
            let stale = projects
                .iter()
                .find(|project| project["plan_count"] == 0)
                .unwrap();
            assert!(!stale["unavailable"].as_array().unwrap().is_empty());
        },
    );
}
