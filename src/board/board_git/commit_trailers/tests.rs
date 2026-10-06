use super::*;
use crate::board::board_ids::PlanId;
use crate::board::board_protocol::CommitPlanLink;
use crate::identity::GitOid;

#[test]
fn vendor_attribution_uses_email_domain_and_retains_model_claim() {
    for (domain, label) in [
        ("Anthropic.com", "claude"),
        ("openai.com", "codex"),
        ("moonshot.ai", "kimi"),
        ("x.ai", "grok"),
        ("google.com", "gemini"),
        ("qwen.ai", "qwen"),
        ("meta.com", "muse"),
    ] {
        let coauthor = parse_coauthor(&format!("Model claim <agent@{domain}>")).unwrap();
        assert_eq!(coauthor.harness.as_str(), label);
        assert_eq!(coauthor.model, "Model claim");
    }
    assert_eq!(
        parse_coauthor("Human <a@openai.com.evil.test>")
            .unwrap()
            .harness
            .as_str(),
        "git:a@openai.com.evil.test"
    );
}

#[test]
fn meta_com_coauthors_map_to_one_muse_identity() {
    // Both Muse addresses seen in the wild share one harness identity.
    for email in ["noreply@meta.com", "muse-spark@meta.com"] {
        let coauthor = parse_coauthor(&format!("Muse Spark <{email}>")).unwrap();
        assert_eq!(coauthor.harness.as_str(), "muse", "{email}");
        assert_eq!(coauthor.email, email);
    }
}

#[test]
fn invalid_utf8_record_is_skipped_without_losing_raw_count() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let oid = "b".repeat(40);
    let valid = format!("\0{oid}\01700000000\0Fixture <f@example.test>\0valid\0Plan: P7\0\0\0\n");
    let mut bytes = valid.as_bytes().to_vec();
    let malformed = format!("\0{oid}\01700000000\0Fixture <f@example.test>\0valid\0");
    bytes.extend_from_slice(malformed.as_bytes());
    bytes.extend_from_slice(b"\xff\0\0\0\n");
    bytes.extend_from_slice(valid.as_bytes());
    let parsed = parse_log(&bytes, &key).unwrap();
    assert_eq!(parsed.record_count, 3);
    assert_eq!(parsed.records.len(), 2);
    assert_eq!(parsed.warnings.len(), 1);
}

#[test]
fn incomplete_framing_is_rejected_without_partial_records() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    assert!(parse_log(b"\0bad\0record", &key).is_err());
    assert!(parse_coauthor("Model <email>").is_err());
}

#[test]
fn malformed_plan_task_value_warns_without_dropping_valid_link() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let oid = "b".repeat(40);
    let bytes = format!(
        "\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0Plan: P7\0Plan-Task: P7.1\x1dPlan-Task: garbage\0\0\n"
    );
    let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
    assert_eq!(parsed.records.len(), 1);
    assert_eq!(
        parsed.records[0].commit.plans,
        vec![CommitPlanLink {
            plan_id: PlanId::new(7).unwrap(),
            task_ordinal: Some(1),
        }]
    );
    assert_eq!(parsed.warnings.len(), 1);
    assert!(
        parsed.warnings[0].contains("malformed Plan-Task"),
        "unexpected warning: {}",
        parsed.warnings[0]
    );
}

#[test]
fn malformed_plan_value_warns_without_dropping_valid_link() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let oid = "b".repeat(40);
    let bytes = format!(
        "\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0Plan: P7\x1dPlan: garbage\0\0\0\n"
    );
    let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
    assert_eq!(parsed.records.len(), 1);
    assert_eq!(
        parsed.records[0].commit.plans,
        vec![CommitPlanLink {
            plan_id: PlanId::new(7).unwrap(),
            task_ordinal: None,
        }]
    );
    assert_eq!(parsed.warnings.len(), 1);
    assert!(
        parsed.warnings[0].contains("malformed Plan:"),
        "unexpected warning: {}",
        parsed.warnings[0]
    );
}

#[test]
fn all_bad_plan_values_still_record_the_commit() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let oid = "b".repeat(40);
    let bytes =
        format!("\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0Plan: garbage\0\0\0\n");
    let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
    assert_eq!(parsed.records.len(), 1);
    assert!(parsed.records[0].commit.plans.is_empty());
    assert!(parsed.records[0].has_plan_trailer);
    assert_eq!(parsed.warnings.len(), 1);
}

#[test]
fn mismatched_plan_task_warns() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let oid = "b".repeat(40);
    let bytes = format!(
        "\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0Plan: P4\0Plan-Task: P3.4\0\0\n"
    );
    let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
    assert_eq!(parsed.records.len(), 1);
    assert_eq!(
        parsed.records[0].commit.plans,
        vec![CommitPlanLink {
            plan_id: PlanId::new(4).unwrap(),
            task_ordinal: None,
        }],
        "a task of another plan never links"
    );
    assert_eq!(parsed.warnings.len(), 1, "{:?}", parsed.warnings);
    assert!(
        parsed.warnings[0].contains("plan_task_without_plan"),
        "unexpected warning: {}",
        parsed.warnings[0]
    );
    assert!(
        parsed.warnings[0].contains(&oid),
        "warning must name the commit: {}",
        parsed.warnings[0]
    );
}

#[test]
fn latin1_author_decodes_lossily_with_warning() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let oid = "b".repeat(40);
    let mut bytes = format!("\0{oid}\01700000000\0").into_bytes();
    bytes.extend_from_slice(b"Caf\xe9 <c@example.test>");
    bytes.extend_from_slice(b"\0subject\0Plan: P7\0\0\0\n");
    let parsed = parse_log(&bytes, &key).unwrap();
    assert_eq!(parsed.records.len(), 1);
    assert_eq!(
        parsed.records[0].commit.author,
        "Caf\u{fffd} <c@example.test>"
    );
    assert!(!parsed.records[0].commit.plans.is_empty());
    assert_eq!(parsed.warnings.len(), 1);
    assert!(
        parsed.warnings[0].contains("author"),
        "unexpected warning: {}",
        parsed.warnings[0]
    );
}

#[test]
fn final_paragraph_trailers_seen_by_git_do_not_warn() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let oid = "b".repeat(40);
    let bytes = format!(
        "\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0Plan: P7\0Plan-Task: P7.3\0Co-authored-by: Model claim <noreply@openai.com>\0\n"
    );
    let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
    assert_eq!(parsed.records.len(), 1);
    assert_eq!(parsed.records[0].commit.plans.len(), 1);
    assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
}

#[test]
fn plan_task_trailer_without_plan_warns_and_stays_unlinked() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let oid = "b".repeat(40);
    let bytes = format!(
        "\0{oid}\01700000000\0Fixture <f@example.test>\0task only\0\0Plan-Task: P7.3\0Co-authored-by: Model claim <noreply@openai.com>\0\n"
    );
    let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
    assert_eq!(parsed.records.len(), 1);
    assert!(
        parsed.records[0].commit.plans.is_empty(),
        "a plan task without a plan never links"
    );
    assert!(!parsed.records[0].has_plan_trailer);
    assert_eq!(parsed.warnings.len(), 1, "{:?}", parsed.warnings);
    assert!(
        parsed.warnings[0].contains("plan_task_without_plan"),
        "unexpected warning: {}",
        parsed.warnings[0]
    );
    assert!(
        parsed.warnings[0].contains(&oid),
        "warning must name the commit: {}",
        parsed.warnings[0]
    );
}

#[test]
fn latin1_coauthor_trailer_decodes_lossily_keeping_the_record() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let oid = "b".repeat(40);
    let mut bytes = format!("\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0Plan: P7\0\0")
        .into_bytes();
    bytes.extend_from_slice(b"Co-authored-by: Caf\xe9 <c@example.test>");
    bytes.extend_from_slice(b"\0\n");
    let parsed = parse_log(&bytes, &key).unwrap();
    assert_eq!(
        parsed.records.len(),
        1,
        "a non-UTF-8 trailer must not drop the commit: {:?}",
        parsed.warnings
    );
    let commit = &parsed.records[0].commit;
    assert_eq!(commit.plans.len(), 1);
    assert_eq!(commit.coauthors.len(), 1);
    assert_eq!(commit.coauthors[0].model, "Caf\u{fffd}");
    assert_eq!(parsed.warnings.len(), 1, "{:?}", parsed.warnings);
    assert!(
        parsed.warnings[0].contains("not valid UTF-8"),
        "unexpected warning: {}",
        parsed.warnings[0]
    );
}

#[test]
fn plan_links_past_limit_truncate_with_warning() {
    let key = RepoKey::from_roots([GitOid::parse(&"a".repeat(40)).unwrap()]).unwrap();
    let oid = "b".repeat(40);
    let plans = (1..=260u64)
        .map(|number| format!("Plan: P{number}"))
        .collect::<Vec<_>>()
        .join("\x1d");
    let bytes = format!("\0{oid}\01700000000\0Fixture <f@example.test>\0subject\0{plans}\0\0\0\n");
    let parsed = parse_log(bytes.as_bytes(), &key).unwrap();
    assert_eq!(parsed.records.len(), 1);
    let links = &parsed.records[0].commit.plans;
    assert_eq!(links.len(), 256);
    assert_eq!(links.first().unwrap().plan_id, PlanId::new(1).unwrap());
    assert_eq!(links.last().unwrap().plan_id, PlanId::new(256).unwrap());
    assert_eq!(parsed.warnings.len(), 1);
    assert!(
        parsed.warnings[0].contains("past the 256 limit"),
        "unexpected warning: {}",
        parsed.warnings[0]
    );
}
