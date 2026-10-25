//! Peer-address provenance columns for Board API v11.

pub(in crate::local_board::board_database) const SCHEMA_V11: &str = r#"
ALTER TABLE entries ADD COLUMN origin_ip TEXT;
ALTER TABLE events ADD COLUMN origin_ip TEXT;
DELETE FROM operation_dedupes;
"#;
