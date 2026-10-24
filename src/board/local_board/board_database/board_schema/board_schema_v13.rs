//! Explicit plan lifecycle and immutable task succession.

pub(in crate::board::local_board::board_database) const SCHEMA_V13: &str = r#"
ALTER TABLE plans ADD COLUMN lifecycle_state TEXT NOT NULL DEFAULT 'open'
 CHECK(lifecycle_state IN ('open','closed'));
CREATE TABLE task_transfers(
 source_plan INTEGER NOT NULL,
 source_ordinal INTEGER NOT NULL,
 destination_plan INTEGER NOT NULL,
 destination_ordinal INTEGER NOT NULL,
 entry_id INTEGER NOT NULL UNIQUE REFERENCES entries(id),
 source_decide_basis TEXT NOT NULL,
 source_decide_entry INTEGER REFERENCES entries(id),
 destination_decide_basis TEXT NOT NULL,
 destination_decide_entry INTEGER REFERENCES entries(id),
 destination_assign_basis TEXT,
 destination_assign_entry INTEGER REFERENCES entries(id),
 PRIMARY KEY(source_plan,source_ordinal),
 UNIQUE(destination_plan,destination_ordinal),
 FOREIGN KEY(source_plan,source_ordinal) REFERENCES tasks(plan_id,ordinal),
 FOREIGN KEY(destination_plan,destination_ordinal) REFERENCES tasks(plan_id,ordinal),
 CHECK(source_plan<>destination_plan),
 CHECK((source_decide_basis='owner' AND source_decide_entry IS NULL) OR
       (source_decide_basis='grant' AND source_decide_entry IS NOT NULL)),
 CHECK((destination_decide_basis='owner' AND destination_decide_entry IS NULL) OR
       (destination_decide_basis='grant' AND destination_decide_entry IS NOT NULL)),
 CHECK((destination_assign_basis IS NULL AND destination_assign_entry IS NULL) OR
       (destination_assign_basis IS NOT NULL AND destination_assign_basis='owner' AND destination_assign_entry IS NULL) OR
       (destination_assign_basis IS NOT NULL AND destination_assign_basis='grant' AND destination_assign_entry IS NOT NULL))
);
CREATE TRIGGER task_transfers_immutable_update BEFORE UPDATE ON task_transfers
 BEGIN SELECT RAISE(ABORT,'task transfer history is immutable'); END;
CREATE TRIGGER task_transfers_immutable_delete BEFORE DELETE ON task_transfers
 BEGIN SELECT RAISE(ABORT,'task transfer history is immutable'); END;
CREATE TRIGGER task_transfers_source_entry BEFORE INSERT ON task_transfers
 WHEN NOT EXISTS(SELECT 1 FROM entries WHERE id=NEW.entry_id
                 AND plan_id=NEW.source_plan AND kind='task_transfer')
 BEGIN SELECT RAISE(ABORT,'task transfer requires its source-plan transfer entry'); END;
CREATE TEMP TABLE entry_authority_v13(id INTEGER PRIMARY KEY,basis TEXT,grant_id INTEGER);
INSERT INTO entry_authority_v13 SELECT id,authority_basis,authority_entry FROM entries;
ALTER TABLE entries DROP COLUMN authority_basis;
UPDATE entries SET authority_entry=NULL;
ALTER TABLE entries ADD COLUMN authority_basis TEXT CHECK(
 (authority_basis IS NULL AND authority_entry IS NULL) OR
 (authority_basis IS NOT NULL AND authority_basis IN ('owner','reporter','creator','stale','holder','delegator') AND authority_entry IS NULL) OR
 (authority_basis IS NOT NULL AND authority_basis='grant' AND authority_entry IS NOT NULL)
);
UPDATE entries SET
 authority_basis=(SELECT basis FROM entry_authority_v13 WHERE id=entries.id),
 authority_entry=(SELECT grant_id FROM entry_authority_v13 WHERE id=entries.id);
DROP TABLE entry_authority_v13;
CREATE TEMP TABLE event_authority_v13(seq INTEGER PRIMARY KEY,basis TEXT,grant_id INTEGER);
INSERT INTO event_authority_v13 SELECT seq,authority_basis,authority_entry FROM events;
ALTER TABLE events DROP COLUMN authority_basis;
UPDATE events SET authority_entry=NULL;
ALTER TABLE events ADD COLUMN authority_basis TEXT CHECK(
 (authority_basis IS NULL AND authority_entry IS NULL) OR
 (authority_basis IS NOT NULL AND authority_basis IN ('owner','reporter','creator','stale','holder','delegator') AND authority_entry IS NULL) OR
 (authority_basis IS NOT NULL AND authority_basis='grant' AND authority_entry IS NOT NULL)
);
UPDATE events SET
 authority_basis=(SELECT basis FROM event_authority_v13 WHERE seq=events.seq),
 authority_entry=(SELECT grant_id FROM event_authority_v13 WHERE seq=events.seq);
DROP TABLE event_authority_v13;
"#;
