use super::*;
use serde_json::{Value, json};
use tp_core::output::{OutputBudget, OutputFormat};

#[test]
fn live_regex_exclusions_preserve_complete_zero_and_positive_coverage() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    fs::write(root.join("accepted.rs"), "fn loc_blind_states() {}\n")?;
    fs::write(root.join("excluded.bin"), b"loc_blind_states\0")?;

    let absent = search(
        &root,
        &cache,
        &Query::parse("re:absent_marker")?,
        &InvocationDirectory::root(),
        &deadline(),
        options(),
    )?;
    assert!(absent.hits.is_empty());
    assert_eq!(absent.coverage["complete"], true);
    assert_eq!(absent.coverage["current_source"]["complete"], true);
    assert_eq!(absent.coverage["current_source"]["excluded_files"], 1);
    assert_eq!(absent.coverage["absence"], "exhaustive");
    assert!(!absent.truncated);

    let present = search(
        &root,
        &cache,
        &Query::parse("re:loc_blind_states")?,
        &InvocationDirectory::root(),
        &deadline(),
        options(),
    )?;
    assert_eq!(present.hits.len(), 1);
    assert_eq!(present.hits[0].path, "accepted.rs");
    assert_eq!(present.coverage["complete"], true);
    assert_eq!(present.coverage["current_source"]["complete"], true);
    assert_eq!(present.coverage["current_source"]["excluded_files"], 1);
    assert!(!present.truncated);
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn live_regex_page_over_twenty_respects_limit_without_a_cursor() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    for index in 0..25 {
        fs::write(
            root.join(format!("match-{index:02}.rs")),
            "fn page_marker() {}\n",
        )?;
    }
    let page_options = CurrentSearchOptions {
        page_limit: 20,
        ..options()
    };
    let result = search(
        &root,
        &cache,
        &Query::parse("re:page_marker")?,
        &InvocationDirectory::root(),
        &deadline(),
        page_options,
    )?;
    assert_eq!(result.hits.len(), 25);
    assert_eq!(result.coverage["complete"], true);
    assert!(!result.truncated);

    let budget = OutputBudget::new(17_000)?.with_format(OutputFormat::Json);
    let encoded = render_with_metadata(&root, None, &result, &json!({}), 20, &budget)?;
    let page: Value = serde_json::from_str(&encoded)?;
    assert_eq!(page["hits"].as_array().unwrap().len(), 20);
    assert_eq!(page["next"], Value::Null);
    assert_eq!(page["truncated"], true);
    assert_eq!(page["coverage"]["complete"], true);
    assert!(!cache.exists());
    Ok(())
}

#[test]
fn live_regex_hit_cap_keeps_partial_coverage_and_explanation() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let (root, cache) = create_root(directory.path())?;
    let matches = "hit_cap_marker\n".repeat(crate::results::MAX_HITS + 1);
    fs::write(root.join("many_matches.rs"), matches)?;

    let result = search(
        &root,
        &cache,
        &Query::parse("re:hit_cap_marker")?,
        &InvocationDirectory::root(),
        &deadline(),
        CurrentSearchOptions {
            page_limit: 20,
            ..options()
        },
    )?;
    assert_eq!(result.hits.len(), 40);
    assert_eq!(result.coverage["complete"], false);
    assert_eq!(result.coverage["current_source"]["complete"], false);
    assert_eq!(result.coverage["current_source"]["limit"], "hit_cap");
    assert_eq!(
        result.coverage["current_source"]["unsearched_unknown"],
        true
    );
    assert!(
        result.coverage["hint"]
            .as_str()
            .unwrap()
            .contains(&format!("{}-hit cap", crate::results::MAX_HITS))
    );
    assert!(result.truncated);
    assert!(!cache.exists());
    Ok(())
}
