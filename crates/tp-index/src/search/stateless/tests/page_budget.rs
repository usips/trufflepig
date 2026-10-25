use super::*;
use serde_json::json;
use tp_core::output::OutputFormat;

const DEFAULT_BUDGET: usize = 600;

fn one_file_regex(root: &Path, cache: &Path, text: &str) -> Result<crate::results::ResultSet> {
    search(
        root,
        cache,
        &Query::parse(text)?,
        &InvocationDirectory::root(),
        &deadline(),
        options(),
    )
}

#[test]
fn one_file_regex_fits_the_default_budget_alone_and_across_six_members() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut members = Vec::with_capacity(6);
    for index in 0..6 {
        let root = directory.path().join(format!("member{index}"));
        fs::create_dir_all(root.join("src"))?;
        fs::write(
            root.join("src/keys.rs"),
            "fn drop_key() { key.forget(); }\n",
        )?;
        for other in 0..20 {
            fs::write(root.join(format!("src/other{other}.rs")), "fn other() {}\n")?;
        }
        members.push(root);
    }
    let cache = directory.path().join("never-created-cache");
    let text = r"re:\.forget\(\) file:src/keys.rs";
    let sets = members
        .iter()
        .map(|root| one_file_regex(root, &cache, text))
        .collect::<Result<Vec<_>>>()?;

    for format in [OutputFormat::Json, OutputFormat::Lines] {
        let budget = OutputBudget::new(DEFAULT_BUDGET)?.with_format(format);
        let single = render_with_metadata(&members[0], None, &sets[0], &json!({}), 10, &budget)?;
        assert!(single.contains("src/keys.rs"), "{single}");

        let rows: Vec<_> = members
            .iter()
            .zip(&sets)
            .enumerate()
            .map(|(index, (root, set))| {
                json!({"member":format!("member{index}"),"root":root,"state":"searched",
                    "source":"current_source","coverage":set.coverage,
                    "truncated":set.truncated,"hits":set.hits.len()})
            })
            .collect();
        let metadata = json!({"workspace":"space","source":"current_source","cache":"unused",
            "home":"member0","scope":"all (no home hits)","coverage":rows,"truncated":false});
        let names: Vec<String> = (0..members.len())
            .map(|index| format!("member{index}"))
            .collect();
        let parts: Vec<_> = members
            .iter()
            .zip(&sets)
            .zip(&names)
            .map(|((root, set), name)| (root.as_path(), Some(name.as_str()), set))
            .collect();
        let page = render_members(&parts, &metadata, 10, &budget)?;
        assert!(page.contains("src/keys.rs"), "{format:?}: {page}");
        assert!(budget.fits(&page));
    }
    assert!(!cache.exists());
    Ok(())
}
