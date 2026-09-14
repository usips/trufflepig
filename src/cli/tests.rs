use super::*;
use crate::search;

#[test]
fn daemon_dispatch_rejects_wrong_root_and_unbounded_options() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let mut options = parse(&[
        "--root".into(),
        other.path().display().to_string(),
        "status".into(),
    ])
    .unwrap();
    assert!(
        local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string()
            .contains("invalid_root")
    );
    options.root = root.path().to_owned();
    options.limit = 0;
    assert!(
        local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string()
            .contains("invalid_limit")
    );
    options.limit = 1;
    options.budget = usize::MAX;
    assert!(
        local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string()
            .contains("invalid_budget")
    );
}

#[test]
fn unknown_commands_are_rejected_before_root_validation() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    for command in ["statuss", "not-a-command"] {
        let options = parse(&[
            "--root".into(),
            other.path().display().to_string(),
            command.into(),
        ])
        .unwrap();
        let error = local(root.path(), cache.path(), &options, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown_command"), "{error}");
        assert!(error.contains("search"), "{error}");
        assert!(!error.contains("invalid_root"), "{error}");
    }
}

#[test]
fn explicit_search_accepts_command_words() {
    let options = parse(&["search".into(), "status".into(), "not-a-command".into()]).unwrap();
    validate(&options).unwrap();
    assert_eq!(
        options.words,
        vec![
            "search".to_owned(),
            "status".to_owned(),
            "not-a-command".to_owned(),
        ]
    );
}

#[test]
fn client_normalization_preserves_regex_spaces() {
    let options = parse(&[
        "search".into(),
        "re:a  b".into(),
        "--budget".into(),
        "500".into(),
    ])
    .unwrap();
    let root = Path::new("/example");
    let forwarded = parse(&normalized_args(&options, root)).unwrap();
    assert_eq!(forwarded.root, root);
    assert_eq!(forwarded.budget, 500);
    assert_eq!(
        search::Query::parse(&forwarded.words[1..].join(" "))
            .unwrap()
            .text,
        "a  b"
    );
}
