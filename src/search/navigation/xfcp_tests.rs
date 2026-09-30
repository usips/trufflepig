use super::*;
use serde_json::Value;
use std::path::Path;

const BASE: &str = "XF\\Permission\\Builder";
const OUTER: &str = "Fixture\\Outer\\XF\\Permission\\Builder";
const INNER: &str = "Fixture\\Inner\\XF\\Permission\\Builder";
const OUTER_PHP: &str = "src/addons/Fixture/Outer/XF/Permission/Builder.php";
const INNER_PHP: &str = "src/addons/Fixture/Inner/XF/Permission/Builder.php";

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn add_extension(root: &Path, addon: &str, base: &str, implementation: &str, order: u32) {
    let directory = format!("src/addons/{addon}");
    write(
        root,
        &format!("{directory}/addon.json"),
        &format!(r#"{{"legacyId":"{addon}"}}"#),
    );
    write(
        root,
        &format!("{directory}/_data/class_extensions.xml"),
        &format!(
            r#"<class_extensions><extension from_class="{base}" to_class="{implementation}" active="1" execute_order="{order}"/></class_extensions>"#
        ),
    );
}

fn fixture() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    let cache = directory.path().join("cache");
    std::fs::create_dir_all(&root).unwrap();
    write(
        &root,
        "src/XF/Mvc/AbstractBuilder.php",
        "<?php namespace XF\\Mvc; class AbstractBuilder {}",
    );
    write(
        &root,
        "src/XF/Permission/Builder.php",
        "<?php namespace XF\\Permission; class Builder extends \\XF\\Mvc\\AbstractBuilder { public function run() {} }",
    );
    write(
        &root,
        "src/Fixture/OtherBase.php",
        "<?php namespace Fixture; class OtherBase { public function other() {} }",
    );
    write(
        &root,
        "src/Fixture/OtherBuilder.php",
        "<?php namespace Fixture; class OtherBuilder extends OtherBase { public function unrelated() { parent::other(); } }",
    );
    write(
        &root,
        OUTER_PHP,
        "<?php namespace Fixture\\Outer\\XF\\Permission; class Builder extends XFCP_Builder { public function run() { parent::run(); } }",
    );
    write(
        &root,
        INNER_PHP,
        "<?php namespace Fixture\\Inner\\XF\\Permission; class Builder extends XFCP_Builder { public function run() { parent::run(); } }",
    );
    add_extension(&root, "Fixture/Outer", BASE, OUTER, 10);
    add_extension(&root, "Fixture/Inner", BASE, INNER, 20);

    write(
        &root,
        "src/XF/Pub/Controller/Forum.php",
        "<?php namespace XF\\Pub\\Controller; class Forum {}",
    );
    for (addon, short) in [("Fixture/TieA", "TieA"), ("Fixture/TieB", "TieB")] {
        let namespace = addon.replace('/', "\\");
        let implementation = format!(r"{namespace}\XF\Pub\Controller\Forum");
        let php_path = format!("src/addons/{addon}/XF/Pub/Controller/Forum.php");
        write(
            &root,
            &php_path,
            &format!(
                "<?php namespace {namespace}\\XF\\Pub\\Controller; class Forum extends XFCP_Forum {{}} // {short}"
            ),
        );
        add_extension(
            &root,
            addon,
            "XF\\Pub\\Controller\\Forum",
            &implementation,
            10,
        );
    }
    write(
        &root,
        "src/Fixture/Orphan.php",
        "<?php namespace Fixture\\Orphan; class Child extends MissingBase {}",
    );

    let mut store = Store::open(&root, &cache).unwrap();
    store.index().unwrap();
    store
        .conn
        .execute(
            "UPDATE definitions SET container='XF\\Permission\\Builder'
             WHERE kind='method' AND name='unrelated'
               AND file_id=(SELECT id FROM files WHERE path='src/Fixture/OtherBuilder.php')",
            [],
        )
        .unwrap();
    (directory, store)
}

fn selected_hit(store: &Store, path: &str, kind: &str, name: &str) -> (i64, Hit) {
    let set = map(store, path).unwrap();
    let hit = set
        .hits
        .into_iter()
        .find(|hit| hit.kind == kind && hit.name == name)
        .unwrap_or_else(|| panic!("missing {kind} {name} in {path}"));
    (set.generation, hit)
}

fn context_json(store: &Store, generation: i64, hit: Hit, tokens: usize) -> Value {
    let budget = OutputBudget::new(tokens).unwrap();
    let output = context_entry(store, generation, hit, &budget, &serde_json::json!({})).unwrap();
    assert!(budget.fits(&output));
    serde_json::from_str(&output).unwrap()
}

fn edges(value: &Value) -> &[Value] {
    value["relationships"]
        .as_array()
        .expect("relationships array")
}

fn has_kind(value: &Value, kind: &str) -> bool {
    edges(value).iter().any(|edge| edge["kind"] == kind)
}

fn mentions_path(value: &Value, path: &str) -> bool {
    edges(value).iter().any(|edge| {
        ["source", "target"]
            .iter()
            .any(|side| edge[*side]["path"].as_str() == Some(path))
    })
}

fn assert_context_shape(value: &Value) {
    let mut envelope_keys = value
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    envelope_keys.sort_unstable();
    assert_eq!(
        envelope_keys,
        vec![
            "generation".to_owned(),
            "hit".to_owned(),
            "relationships".to_owned(),
            "tokenizer".to_owned(),
            "truncated".to_owned(),
        ]
    );
    for edge in edges(value) {
        let mut keys = edge
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "end".to_owned(),
                "kind".to_owned(),
                "path".to_owned(),
                "provenance".to_owned(),
                "resolution".to_owned(),
                "source".to_owned(),
                "start".to_owned(),
                "target".to_owned(),
            ]
        );
    }
}

#[test]
fn context_follows_framework_and_php_parents_from_class_method_metadata_and_placeholder_hits() {
    let (_directory, store) = fixture();
    let foreign_parent_call: bool = store
        .conn
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM definitions d JOIN files f ON f.id=d.file_id
                 JOIN relationships r ON r.source=d.id
                 WHERE f.path=?1 AND d.kind='method' AND d.name='unrelated'
                   AND r.kind='php_parent_call_candidate')",
            ["src/Fixture/OtherBuilder.php"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(foreign_parent_call);

    let (generation, inner_class) = selected_hit(&store, INNER_PHP, "class", "Builder");
    let class_context = context_json(&store, generation, inner_class.clone(), 6000);
    assert!(has_kind(&class_context, "framework_parent_candidate"));
    assert!(mentions_path(&class_context, OUTER_PHP));
    assert!(!mentions_path(
        &class_context,
        "src/Fixture/OtherBuilder.php"
    ));
    assert_context_shape(&class_context);

    let (method_generation, inner_method) = selected_hit(&store, INNER_PHP, "method", "run");
    assert_eq!(method_generation, generation);
    let method_context = context_json(&store, generation, inner_method, 6000);
    assert!(has_kind(&method_context, "php_parent_call_candidate"));
    assert!(mentions_path(&method_context, OUTER_PHP));

    let metadata_path = "src/addons/Fixture/Inner/_data/class_extensions.xml";
    let (metadata_generation, metadata_hit) =
        selected_hit(&store, metadata_path, "xenforo_class_extension", INNER);
    assert_eq!(metadata_generation, generation);
    let metadata_context = context_json(&store, generation, metadata_hit, 6000);
    assert!(mentions_path(&metadata_context, INNER_PHP));

    let placeholders = references(&store, &reference_query("XFCP_Builder").unwrap()).unwrap();
    let placeholder = placeholders
        .hits
        .into_iter()
        .find(|hit| hit.path == INNER_PHP)
        .expect("inner generated-parent occurrence");
    assert_eq!(placeholder.resolution.as_deref(), Some("candidate"));
    assert!(placeholder.target.is_none());
    assert!(
        placeholder
            .candidates
            .iter()
            .any(|candidate| candidate.path == OUTER_PHP)
    );
    let placeholder_json = serde_json::to_value(&placeholder).unwrap();
    assert_eq!(placeholder_json["resolution"], "candidate");
    assert!(placeholder_json.get("target").is_none());
    assert!(
        !placeholder_json["candidates"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let placeholder_context = context_json(&store, generation, placeholder, 6000);
    assert!(has_kind(&placeholder_context, "framework_parent_candidate"));
    assert!(mentions_path(&placeholder_context, OUTER_PHP));

    let (base_generation, base_hit) =
        selected_hit(&store, "src/XF/Permission/Builder.php", "class", "Builder");
    assert_eq!(base_generation, generation);
    let base_context = context_json(&store, generation, base_hit, 6000);
    assert!(has_kind(&base_context, "php_extends_candidate"));
    assert!(mentions_path(
        &base_context,
        "src/XF/Mvc/AbstractBuilder.php"
    ));
    assert!(mentions_path(&base_context, OUTER_PHP));
    assert!(mentions_path(&base_context, INNER_PHP));

    let unresolved = references(
        &store,
        &reference_query("Fixture\\Orphan\\MissingBase").unwrap(),
    )
    .unwrap();
    let unresolved = unresolved
        .hits
        .into_iter()
        .find(|hit| hit.path == "src/Fixture/Orphan.php")
        .expect("missing parent occurrence");
    assert_eq!(unresolved.resolution.as_deref(), Some("unresolved"));
    assert!(unresolved.target.is_none());
    assert!(unresolved.candidates.is_empty());
    let unresolved_json = serde_json::to_value(unresolved).unwrap();
    assert_eq!(unresolved_json["resolution"], "unresolved");
    assert!(unresolved_json.get("target").is_none());
    assert!(unresolved_json.get("candidates").is_none());

    let (orphan_generation, orphan_class) =
        selected_hit(&store, "src/Fixture/Orphan.php", "class", "Child");
    assert_eq!(orphan_generation, generation);
    let orphan_context = context_json(&store, generation, orphan_class, 6000);
    let issue = edges(&orphan_context)
        .iter()
        .find(|edge| edge["kind"] == "inheritance_issue")
        .expect("unresolved inheritance issue");
    assert_eq!(issue["resolution"], "unresolved");
    assert!(issue["target"].is_null());

    let synthetic_proxy_exists: bool = store
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM definitions WHERE kind='class' AND name LIKE 'XFCP_%')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!synthetic_proxy_exists);
}

#[test]
fn tied_framework_parents_are_repeatable_candidates_without_an_order_claim() {
    let (_directory, store) = fixture();
    let (generation, base_hit) =
        selected_hit(&store, "src/XF/Pub/Controller/Forum.php", "class", "Forum");
    let first = context_json(&store, generation, base_hit.clone(), 6000);
    let second = context_json(&store, generation, base_hit, 6000);
    assert_eq!(first, second);
    assert!(has_kind(&first, "framework_parent_candidate"));
    assert!(mentions_path(
        &first,
        "src/addons/Fixture/TieA/XF/Pub/Controller/Forum.php"
    ));
    assert!(mentions_path(
        &first,
        "src/addons/Fixture/TieB/XF/Pub/Controller/Forum.php"
    ));
    assert_context_shape(&first);
}

#[test]
fn inherited_evidence_precedes_context_noise_and_relationships_are_bounded() {
    let (_directory, store) = fixture();
    let (generation, hit) = selected_hit(&store, INNER_PHP, "class", "Builder");
    let (source, file_id): (i64, i64) = store
        .conn
        .query_row(
            "SELECT d.id,d.file_id FROM definitions d JOIN files f ON f.id=d.file_id WHERE f.path=?1 AND d.kind='class' AND d.name='Builder'",
            [INNER_PHP],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    for index in 0..120 {
        store
            .conn
            .execute(
                "INSERT INTO relationships(source,target,kind,file_id,start,end,provenance) VALUES(?1,NULL,'context_noise',?2,?3,?4,?5)",
                rusqlite::params![source, file_id, hit.start as i64, hit.end as i64, format!("fixture_noise_{index}")],
            )
            .unwrap();
    }

    let value = context_json(&store, generation, hit, 100_000);
    assert!(has_kind(&value, "framework_parent_candidate"));
    let kinds: Vec<_> = edges(&value)
        .iter()
        .map(|edge| edge["kind"].as_str().unwrap())
        .collect();
    let inherited = kinds
        .iter()
        .position(|kind| *kind == "framework_parent_candidate")
        .unwrap();
    let noise = kinds
        .iter()
        .position(|kind| *kind == "context_noise")
        .unwrap();
    assert!(inherited < noise, "inherited evidence must survive first");

    let value = context_json(
        &store,
        generation,
        selected_hit(&store, INNER_PHP, "class", "Builder").1,
        100_000,
    );
    assert!(edges(&value).len() <= 100);
    assert_eq!(value["truncated"], true);
}

#[test]
fn context_rejects_stale_generation_and_source_revision() {
    let (_directory, store) = fixture();
    let (generation, hit) = selected_hit(&store, INNER_PHP, "class", "Builder");
    let budget = OutputBudget::new(6000).unwrap();
    let stale_generation = context_entry(
        &store,
        generation - 1,
        hit.clone(),
        &budget,
        &serde_json::json!({}),
    )
    .unwrap_err();
    assert!(stale_generation.to_string().contains("stale_result"));

    let mut stale_source = hit;
    stale_source.revision = Some("old-source-revision".into());
    let stale_source = context_entry(
        &store,
        generation,
        stale_source,
        &budget,
        &serde_json::json!({}),
    )
    .unwrap_err();
    assert!(stale_source.to_string().contains("stale_result"));
}
