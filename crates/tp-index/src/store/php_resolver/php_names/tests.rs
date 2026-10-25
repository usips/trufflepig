use super::{class_index, resolve};
use rusqlite::{Connection, params};

fn fixture() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    crate::store::schema::create(&connection).unwrap();
    connection
}

fn add_file(connection: &Connection, path: &str) -> i64 {
    connection
        .execute(
            "INSERT INTO files(path,language,status,bytes) VALUES(?1,'php','complete',128)",
            [path],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn add_class(connection: &Connection, path: &str, namespace: &str, name: &str) -> i64 {
    let file_id = add_file(connection, path);
    connection
        .execute(
            "INSERT INTO definitions(file_id,name,kind,start,end,container) VALUES(?1,?2,'class',0,128,?3)",
            params![file_id, name, namespace],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn add_owned_occurrence(
    connection: &Connection,
    path: &str,
    name: &str,
    role: &str,
    provenance: &str,
    start: i64,
    end: i64,
) -> i64 {
    let file_id = add_file(connection, path);
    connection
        .execute(
            "INSERT INTO definitions(file_id,name,kind,start,end,container) VALUES(?1,'Source','class',0,128,'Fixture')",
            [file_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO occurrences(file_id,name,start,end,role,target,candidates,provenance) VALUES(?1,?2,?3,?4,?5,NULL,'[]',?6)",
            params![file_id, name, start, end, role, provenance],
        )
        .unwrap();
    connection.last_insert_rowid()
}

fn resolve_fixture(connection: &mut Connection) {
    let index = class_index(connection).unwrap();
    resolve(connection, &index).unwrap();
}

fn occurrence(connection: &Connection, id: i64) -> (Option<i64>, Vec<i64>, String) {
    connection
        .query_row(
            "SELECT target,candidates,provenance FROM occurrences WHERE id=?1",
            [id],
            |row| {
                let candidates: String = row.get(1)?;
                Ok((
                    row.get(0)?,
                    serde_json::from_str(&candidates).unwrap(),
                    row.get(2)?,
                ))
            },
        )
        .unwrap()
}

#[test]
fn unique_import_and_absolute_type_matches_remain_candidates() {
    let mut connection = fixture();
    let imported = add_class(
        &connection,
        "src/addons/Target/Imported.php",
        "Acme\\Library",
        "ImportedService",
    );
    let absolute = add_class(
        &connection,
        "src/addons/Target/Absolute.php",
        "Acme\\Library",
        "AbsoluteService",
    );
    let import_occurrence = add_owned_occurrence(
        &connection,
        "src/addons/Source/Import.php",
        "Acme\\Library\\ImportedService",
        "import",
        "php_import",
        20,
        40,
    );
    let type_occurrence = add_owned_occurrence(
        &connection,
        "src/addons/Source/Type.php",
        "\\Acme\\Library\\AbsoluteService",
        "type",
        "php_fqcn",
        30,
        50,
    );

    resolve_fixture(&mut connection);

    let (import_target, import_candidates, import_provenance) =
        occurrence(&connection, import_occurrence);
    assert_eq!(import_target, None);
    assert_eq!(import_candidates, vec![imported]);
    assert!(import_provenance.starts_with("php_import;php_fqcn_candidate_lookup"));
    let (type_target, type_candidates, type_provenance) = occurrence(&connection, type_occurrence);
    assert_eq!(type_target, None);
    assert_eq!(type_candidates, vec![absolute]);
    assert!(type_provenance.starts_with("php_fqcn;php_fqcn_candidate_lookup"));

    let relationship_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM relationships WHERE kind IN ('php_import_candidate','php_type_candidate') AND target IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(relationship_count, 2);
}

#[test]
fn fqcn_lookup_ignores_ascii_case() {
    let mut connection = fixture();
    let target = add_class(
        &connection,
        "src/addons/Target/Gateway.php",
        "Vendor\\Gateway",
        "PaymentGateway",
    );
    let occurrence_id = add_owned_occurrence(
        &connection,
        "src/addons/Source/Checkout.php",
        "\\vendor\\gateway\\paymentgateway",
        "type",
        "php_fqcn",
        32,
        49,
    );

    resolve_fixture(&mut connection);

    assert_eq!(occurrence(&connection, occurrence_id).1, vec![target]);
}

#[test]
fn function_and_constant_imports_are_excluded() {
    let mut connection = fixture();
    add_class(
        &connection,
        "src/addons/Target/Imported.php",
        "Acme\\Library",
        "ImportedService",
    );
    let function_import = add_owned_occurrence(
        &connection,
        "src/addons/Source/FunctionImport.php",
        "Acme\\Library\\ImportedService",
        "import",
        "php_function_import",
        20,
        40,
    );
    let const_import = add_owned_occurrence(
        &connection,
        "src/addons/Source/ConstImport.php",
        "Acme\\Library\\ImportedService",
        "import",
        "php_const_import",
        21,
        41,
    );

    resolve_fixture(&mut connection);

    for id in [function_import, const_import] {
        let (target, candidates, provenance) = occurrence(&connection, id);
        assert_eq!(target, None);
        assert!(candidates.is_empty());
        assert!(!provenance.contains("php_fqcn_candidate_lookup"));
    }
}

#[test]
fn candidate_lists_are_capped_and_marked_truncated() {
    let mut connection = fixture();
    for ordinal in 0..65 {
        add_class(
            &connection,
            &format!("src/addons/Duplicate/{ordinal}.php"),
            "Acme\\Duplicate",
            "SharedService",
        );
    }
    let occurrence_id = add_owned_occurrence(
        &connection,
        "src/addons/Source/Consumer.php",
        "Acme\\Duplicate\\SharedService",
        "type",
        "php_fqcn",
        24,
        47,
    );

    resolve_fixture(&mut connection);

    let (target, candidates, provenance) = occurrence(&connection, occurrence_id);
    assert_eq!(target, None);
    assert_eq!(candidates.len(), 64);
    assert!(provenance.contains("php_candidates_truncated"));
}
