use super::resolve;
use rusqlite::{Connection, OptionalExtension, params};

fn fixture() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    crate::store::schema::create(&connection).unwrap();
    connection
}

fn add_file(connection: &Connection, path: &str, source: &str) -> i64 {
    let revision = format!("fixture:{path}");
    connection
        .execute(
            "INSERT INTO contents(revision,bytes) VALUES(?1,?2)",
            params![revision, source.as_bytes()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO files(path,revision,language,status,bytes) VALUES(?1,?2,?3,'complete',?4)",
            params![
                path,
                revision,
                if path.ends_with(".php") {
                    "php"
                } else {
                    "text"
                },
                source.len() as i64
            ],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn add_addon(connection: &Connection, path: &str, id: &str, requirements: &str) {
    let source = format!(r#"{{"legacyId":"{id}","require":{requirements}}}"#);
    add_file(connection, path, &source);
}

fn add_class(connection: &Connection, path: &str, namespace: &str, name: &str) -> i64 {
    let source = format!("<?php\nnamespace {namespace};\nclass {name} {{}}\n");
    let start = source.find("class ").unwrap() as i64;
    let end = source.find("}\n").unwrap() as i64 + 1;
    let file_id = add_file(connection, path, &source);
    connection.execute(
        "INSERT INTO definitions(file_id,name,kind,start,end,container) VALUES(?1,?2,'class',?3,?4,?5)",
        params![file_id, name, start, end, namespace],
    ).unwrap();
    connection.last_insert_rowid()
}

fn definition(
    connection: &Connection,
    path: &str,
    kind: &str,
    name: &str,
) -> Option<(i64, Option<String>, i64, i64)> {
    connection.query_row(
        "SELECT d.id,d.container,d.start,d.end FROM definitions d JOIN files f ON f.id=d.file_id WHERE f.path=?1 AND d.kind=?2 AND d.name=?3",
        params![path, kind, name],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).optional().unwrap()
}

fn relationships(
    connection: &Connection,
    path: &str,
    kind: &str,
) -> Vec<(i64, Option<i64>, i64, i64)> {
    let mut statement = connection.prepare(
        "SELECT r.source,r.target,r.start,r.end FROM relationships r JOIN files f ON f.id=r.file_id WHERE f.path=?1 AND r.kind=?2 ORDER BY r.target",
    ).unwrap();
    statement
        .query_map(params![path, kind], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn occurrence_count(connection: &Connection, path: &str) -> i64 {
    connection
        .query_row(
            "SELECT count(*) FROM occurrences o JOIN files f ON f.id=o.file_id WHERE f.path=?1",
            [path],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn addon_manifests_define_ids_and_preserve_requirement_candidates() {
    let mut connection = fixture();
    let source_path = "src/addons/HappyBoard/ThreeHundred/addon.json";
    let target_path = "src/addons/Vendor/Library/addon.json";
    add_addon(
        &connection,
        source_path,
        "HappyBoard/ThreeHundred",
        r#"{"Vendor/Library":"1.2.0","Missing/AddOn":"1.0.0","XF":2030000,"Vendor/Duplicate":"1.0.0"}"#,
    );
    add_addon(&connection, target_path, "Vendor/Library", "{}");
    let duplicate_paths = [
        "src/addons/Vendor/Duplicate/addon.json",
        "src/addons/Vendor/%44uplicate/addon.json",
    ];
    for path in duplicate_paths {
        add_addon(&connection, path, "Vendor/Duplicate", "{}");
    }

    resolve(&mut connection).unwrap();

    let (source_id, container, start, end) =
        definition(&connection, source_path, "addon", "HappyBoard/ThreeHundred")
            .expect("valid addon.json defines its directory ID");
    assert_eq!(container, None);
    let manifest_bytes: i64 = connection
        .query_row(
            "SELECT bytes FROM files WHERE path=?1",
            [source_path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((start, end), (0, manifest_bytes));

    let target_id = definition(&connection, target_path, "addon", "Vendor/Library")
        .unwrap()
        .0;
    let local = relationships(
        &connection,
        source_path,
        "xenforo_addon_requirement_candidate",
    );
    assert_eq!(local.len(), 3);
    assert!(
        local
            .iter()
            .any(|edge| edge.0 == source_id && edge.1 == Some(target_id))
    );
    let unresolved = relationships(&connection, source_path, "xenforo_addon_requirement");
    assert_eq!(unresolved.len(), 2);
    assert!(
        unresolved
            .iter()
            .all(|edge| edge.0 == source_id && edge.1.is_none())
    );

    let duplicate_ids = duplicate_paths.map(|path| {
        definition(&connection, path, "addon", "Vendor/Duplicate")
            .expect("each encoded path is retained as a definition")
            .0
    });
    let mut actual = relationships(
        &connection,
        source_path,
        "xenforo_addon_requirement_candidate",
    )
    .into_iter()
    .filter_map(|edge| edge.1)
    .collect::<Vec<_>>();
    actual.sort_unstable();
    let mut expected = vec![target_id, duplicate_ids[0], duplicate_ids[1]];
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

#[test]
fn active_extensions_use_exact_class_owners_and_full_tag_spans() {
    let mut connection = fixture();
    let owner_path = "src/addons/HappyBoard/ThreeHundred/addon.json";
    let base_path = "src/addons/BS/BtcPayProvider/addon.json";
    let wrong_owner_path = "src/addons/Vendor/Expected/addon.json";
    add_addon(&connection, owner_path, "HappyBoard/ThreeHundred", "{}");
    add_addon(&connection, base_path, "BS/BtcPayProvider", "{}");
    add_addon(&connection, wrong_owner_path, "Vendor/Expected", "{}");
    let impl_id = add_class(
        &connection,
        "src/addons/HappyBoard/ThreeHundred/Impl.php",
        r"HappyBoard\ThreeHundred",
        "Impl",
    );
    add_class(
        &connection,
        "src/addons/HappyBoard/ThreeHundred/UnmatchedImpl.php",
        r"HappyBoard\ThreeHundred",
        "UnmatchedImpl",
    );
    let wrong_base_id = add_class(
        &connection,
        "src/addons/BS/BtcPayProvider/Base.php",
        r"BS\BtcPayProvider",
        "Base",
    );
    add_class(
        &connection,
        "src/addons/Vendor/Expected/Base.php",
        r"Vendor\Different",
        "Base",
    );
    let xml_path = "src/addons/HappyBoard/ThreeHundred/_data/class_extensions.xml";
    let tag = r#"<extension from_class="BS\BtcPayProvider\Base" to_class="HappyBoard\ThreeHundred\Impl" active="1" execute_order="20"/>"#;
    let unmatched = r#"<extension from_class="Vendor\Expected\Base" to_class="HappyBoard\ThreeHundred\UnmatchedImpl" active="1" execute_order="21"/>"#;
    let xml = format!("<class_extensions>{tag}{unmatched}</class_extensions>");
    add_file(&connection, xml_path, &xml);

    resolve(&mut connection).unwrap();

    let (extension_id, owner, _, _) = definition(
        &connection,
        xml_path,
        "xenforo_class_extension",
        r"HappyBoard\ThreeHundred\Impl",
    )
    .expect("extension declaration is retained");
    assert_eq!(owner.as_deref(), Some("HappyBoard/ThreeHundred"));
    // Both metadata rows retain their full tag spans.
    let definitions = {
        let mut statement = connection.prepare(
            "SELECT d.start,d.end FROM definitions d JOIN files f ON f.id=d.file_id WHERE f.path=?1 AND d.kind='xenforo_class_extension' ORDER BY d.start",
        ).unwrap();
        statement
            .query_map([xml_path], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .map(|row| row.unwrap())
            .collect::<Vec<_>>()
    };
    let first_start = xml.find(tag).unwrap() as i64;
    let second_start = xml.find(unmatched).unwrap() as i64;
    assert_eq!(
        definitions,
        vec![
            (first_start, first_start + tag.len() as i64),
            (second_start, second_start + unmatched.len() as i64),
        ]
    );
    let class_edges = relationships(&connection, xml_path, "xenforo_class_extension_candidate");
    let base_span = (first_start, first_start + tag.len() as i64);
    assert_eq!(class_edges.len(), 2);
    assert!(class_edges.contains(&(extension_id, Some(wrong_base_id), base_span.0, base_span.1)));
    assert!(class_edges.contains(&(impl_id, Some(wrong_base_id), base_span.0, base_span.1)));
    let owner_id = definition(&connection, owner_path, "addon", "HappyBoard/ThreeHundred")
        .unwrap()
        .0;
    let base_addon_id = definition(&connection, base_path, "addon", "BS/BtcPayProvider")
        .unwrap()
        .0;
    assert_eq!(
        relationships(
            &connection,
            xml_path,
            "xenforo_addon_extension_dependency_candidate"
        ),
        vec![(
            owner_id,
            Some(base_addon_id),
            first_start,
            first_start + tag.len() as i64
        )],
    );
}

#[test]
fn inactive_extensions_keep_metadata_without_candidate_edges() {
    let mut connection = fixture();
    add_addon(
        &connection,
        "src/addons/Owner/Addon/addon.json",
        "Owner/Addon",
        "{}",
    );
    add_addon(
        &connection,
        "src/addons/Base/Addon/addon.json",
        "Base/Addon",
        "{}",
    );
    add_class(
        &connection,
        "src/addons/Owner/Addon/Impl.php",
        r"Owner\Addon",
        "Impl",
    );
    add_class(
        &connection,
        "src/addons/Base/Addon/Base.php",
        r"Base\Addon",
        "Base",
    );
    let path = "src/addons/Owner/Addon/_data/class_extensions.xml";
    let tag = r#"<extension from_class="Base\Addon\Base" to_class="Owner\Addon\Impl" active="0" execute_order="1"/>"#;
    add_file(
        &connection,
        path,
        &format!("<class_extensions>{tag}</class_extensions>"),
    );

    resolve(&mut connection).unwrap();

    assert!(
        definition(
            &connection,
            path,
            "xenforo_class_extension",
            r"Owner\Addon\Impl"
        )
        .is_some()
    );
    assert_eq!(occurrence_count(&connection, path), 2);
    assert!(relationships(&connection, path, "xenforo_class_extension_candidate").is_empty());
    assert!(
        relationships(
            &connection,
            path,
            "xenforo_addon_extension_dependency_candidate"
        )
        .is_empty()
    );
}

#[test]
fn malformed_metadata_files_publish_no_partial_facts() {
    let mut connection = fixture();
    let manifest_path = "src/addons/Broken/Manifest/addon.json";
    add_file(
        &connection,
        manifest_path,
        r#"{"require":{"Vendor/Dependency":"1.0.0"},"#,
    );
    let duplicate_require_path = "src/addons/Broken/DuplicateRequire/addon.json";
    add_file(
        &connection,
        duplicate_require_path,
        r#"{"require":{"Vendor/Dependency":"1"},"require":[]}"#,
    );
    let xml_path = "src/addons/Owner/Addon/_data/class_extensions.xml";
    let valid = r#"<extension from_class="Base\Addon\Base" to_class="Owner\Addon\Impl" active="1" execute_order="1"/>"#;
    add_file(
        &connection,
        xml_path,
        &format!("<class_extensions>{valid}<unfinished>"),
    );

    resolve(&mut connection).unwrap();

    assert!(definition(&connection, manifest_path, "addon", "Broken/Manifest").is_none());
    assert_eq!(occurrence_count(&connection, manifest_path), 0);
    assert!(
        relationships(
            &connection,
            manifest_path,
            "xenforo_addon_requirement_candidate"
        )
        .is_empty()
    );
    assert!(relationships(&connection, manifest_path, "xenforo_addon_requirement").is_empty());
    assert!(
        definition(
            &connection,
            duplicate_require_path,
            "addon",
            "Broken/DuplicateRequire"
        )
        .is_none()
    );
    assert_eq!(occurrence_count(&connection, duplicate_require_path), 0);
    assert!(
        relationships(
            &connection,
            duplicate_require_path,
            "xenforo_addon_requirement_candidate"
        )
        .is_empty()
    );
    assert!(
        relationships(
            &connection,
            duplicate_require_path,
            "xenforo_addon_requirement"
        )
        .is_empty()
    );
    assert!(
        definition(
            &connection,
            xml_path,
            "xenforo_class_extension",
            r"Owner\Addon\Impl"
        )
        .is_none()
    );
    assert_eq!(occurrence_count(&connection, xml_path), 0);
    assert!(relationships(&connection, xml_path, "xenforo_class_extension_candidate").is_empty());
}
