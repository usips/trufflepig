//! Request actor attribution for retained write results.

pub(in crate::board::local_board::board_database) const SCHEMA_V12: &str = r#"
ALTER TABLE operation_dedupes ADD COLUMN actor_id INTEGER REFERENCES actors(id);
"#;
