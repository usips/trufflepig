use super::super::*;
use crate::search;

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
fn negative_path_filter_is_quoted_inside_the_query_argument() {
    let options = parse(&["search".into(), "needle -file:tests".into()]).unwrap();
    let query = search::Query::parse(&options.words[1..].join(" ")).unwrap();
    assert_eq!(query.text, "needle");
    assert_eq!(query.path.excludes(), ["tests"]);

    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    let query = search::Query::parse(&forwarded.words[1..].join(" ")).unwrap();
    assert_eq!(query.path.excludes(), ["tests"]);

    assert!(parse(&["search".into(), "needle".into(), "-file:tests".into()]).is_err());
    assert!(parse(&["search".into(), "needle".into(), "--bogus".into()]).is_err());
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

#[test]
fn client_normalization_forwards_semantic_overrides() {
    let options = parse(&["--sem".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(forwarded.sem);
    assert!(!forwarded.no_sem);

    let options = parse(&["--no-sem".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(!forwarded.sem);
    assert!(forwarded.no_sem);
}

#[test]
fn semantic_flags_are_mutually_exclusive() {
    let error = parse(&["--sem".into(), "--no-sem".into(), "search".into()]).unwrap_err();
    assert!(error.to_string().contains("cannot be used with"));
}

#[test]
fn explicit_budget_is_recorded_only_when_passed() {
    let implicit = parse(&["search".into(), "query".into()]).unwrap();
    assert!(!implicit.explicit_budget);
    assert_eq!(implicit.budget, 600);
    let long = parse(&[
        "--budget".into(),
        "900".into(),
        "search".into(),
        "query".into(),
    ])
    .unwrap();
    assert!(long.explicit_budget);
    let short = parse(&["-b".into(), "900".into(), "search".into(), "query".into()]).unwrap();
    assert!(short.explicit_budget);
    let joined = parse(&["-b900".into(), "search".into(), "query".into()]).unwrap();
    assert!(joined.explicit_budget);
    assert_eq!(joined.budget, 900);
    // Source reads default to a larger budget unless one is given.
    let show = parse(&["show".into(), "path:a.rs".into()]).unwrap();
    assert_eq!(show.budget, SHOW_BUDGET);
    let small = parse(&["-b".into(), "300".into(), "show".into(), "x".into()]).unwrap();
    assert_eq!(small.budget, 300);
}

#[test]
fn client_normalization_forwards_rerank_overrides() {
    let options = parse(&["--rerank".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(forwarded.rerank);
    assert!(!forwarded.no_rerank);

    let options = parse(&["--no-rerank".into(), "search".into(), "query".into()]).unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert!(!forwarded.rerank);
    assert!(forwarded.no_rerank);
}

#[test]
fn rerank_flags_are_mutually_exclusive() {
    let error = parse(&["--rerank".into(), "--no-rerank".into(), "search".into()]).unwrap_err();
    assert!(error.to_string().contains("cannot be used with"));
}

#[test]
fn client_normalization_forwards_format() {
    let options = parse(&[
        "--format".into(),
        "lines".into(),
        "search".into(),
        "q".into(),
    ])
    .unwrap();
    let forwarded = parse(&normalized_args(&options, Path::new("/example"))).unwrap();
    assert_eq!(forwarded.format, "lines");
    assert_eq!(
        forwarded.output_format(),
        crate::output::OutputFormat::Lines
    );
    let implicit = parse(&["search".into(), "q".into()]).unwrap();
    assert_eq!(implicit.output_format(), crate::output::OutputFormat::Json);
}

#[test]
fn json_flag_conflicts_with_lines_format() {
    let options = parse(&[
        "--json".into(),
        "--format".into(),
        "lines".into(),
        "status".into(),
    ])
    .unwrap();
    assert!(
        validate(&options)
            .unwrap_err()
            .to_string()
            .contains("--json conflicts with --format lines")
    );
    assert!(parse(&["--format".into(), "yaml".into(), "status".into()]).is_err());
}
