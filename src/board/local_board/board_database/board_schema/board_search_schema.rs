//! Atomic external-content FTS creation, source triggers, and durable target rebuild.

pub(in crate::board::local_board::board_database) const SCHEMA_V3: &str = r#"
CREATE TABLE search_documents(
 rowid INTEGER PRIMARY KEY, target TEXT NOT NULL UNIQUE,
 plan_id INTEGER REFERENCES plans(id),
 source TEXT NOT NULL CHECK(source IN ('entry','revision','proposal')), body TEXT NOT NULL
);
CREATE INDEX search_documents_plan ON search_documents(plan_id);
CREATE VIRTUAL TABLE board_text USING fts5(
 body, content='search_documents', content_rowid='rowid', tokenize='unicode61'
);
CREATE TRIGGER search_documents_insert AFTER INSERT ON search_documents BEGIN
 INSERT INTO board_text(rowid,body) VALUES(new.rowid,new.body);
END;
CREATE TRIGGER search_documents_delete AFTER DELETE ON search_documents BEGIN
 INSERT INTO board_text(board_text,rowid,body) VALUES('delete',old.rowid,old.body);
END;
CREATE TRIGGER search_documents_update AFTER UPDATE ON search_documents BEGIN
 INSERT INTO board_text(board_text,rowid,body) VALUES('delete',old.rowid,old.body);
 INSERT INTO board_text(rowid,body) VALUES(new.rowid,new.body);
END;
CREATE TRIGGER search_entries_insert AFTER INSERT ON entries BEGIN
 INSERT INTO search_documents(target,plan_id,source,body)
 VALUES('E'||new.id,new.plan_id,'entry',new.body);
END;
CREATE TRIGGER search_entries_update AFTER UPDATE OF body,plan_id ON entries BEGIN
 UPDATE search_documents SET plan_id=new.plan_id,
 source=CASE WHEN EXISTS(SELECT 1 FROM proposals WHERE entry_id=new.id) THEN 'proposal' ELSE 'entry' END,
 body=new.body||COALESCE((SELECT char(10)||t.body FROM proposals p JOIN texts t ON t.hash=p.text_hash
 WHERE p.entry_id=new.id),'')
 WHERE target='E'||new.id;
END;
CREATE TRIGGER search_entries_delete AFTER DELETE ON entries BEGIN
 DELETE FROM search_documents WHERE target='E'||old.id;
END;
CREATE TRIGGER search_proposals_insert AFTER INSERT ON proposals BEGIN
 UPDATE search_documents SET plan_id=new.plan_id,source='proposal',
 body=(SELECT body FROM entries WHERE id=new.entry_id)||char(10)||(SELECT body FROM texts WHERE hash=new.text_hash)
 WHERE target='E'||new.entry_id;
END;
CREATE TRIGGER search_proposals_update AFTER UPDATE OF text_hash,plan_id ON proposals BEGIN
 UPDATE search_documents SET plan_id=new.plan_id,source='proposal',
 body=(SELECT body FROM entries WHERE id=new.entry_id)||char(10)||(SELECT body FROM texts WHERE hash=new.text_hash)
 WHERE target='E'||new.entry_id;
END;
CREATE TRIGGER search_proposals_delete AFTER DELETE ON proposals BEGIN
 UPDATE search_documents SET source='entry',body=(SELECT body FROM entries WHERE id=old.entry_id)
 WHERE target='E'||old.entry_id;
END;
CREATE TRIGGER search_revisions_insert AFTER INSERT ON revisions BEGIN
 INSERT INTO search_documents(target,plan_id,source,body)
 VALUES('P'||new.plan_id||'@'||new.number,new.plan_id,'revision',(SELECT body FROM texts WHERE hash=new.text_hash));
END;
CREATE TRIGGER search_revisions_update AFTER UPDATE OF text_hash ON revisions BEGIN
 UPDATE search_documents SET body=(SELECT body FROM texts WHERE hash=new.text_hash)
 WHERE target='P'||new.plan_id||'@'||new.number;
END;
CREATE TRIGGER search_revisions_delete AFTER DELETE ON revisions BEGIN
 DELETE FROM search_documents WHERE target='P'||old.plan_id||'@'||old.number;
END;
INSERT INTO search_documents(target,plan_id,source,body)
 SELECT 'E'||e.id,e.plan_id,CASE WHEN p.entry_id IS NULL THEN 'entry' ELSE 'proposal' END,
 e.body||COALESCE(char(10)||t.body,'') FROM entries e
 LEFT JOIN proposals p ON p.entry_id=e.id LEFT JOIN texts t ON t.hash=p.text_hash ORDER BY e.id;
INSERT INTO search_documents(target,plan_id,source,body)
 SELECT 'P'||r.plan_id||'@'||r.number,r.plan_id,'revision',t.body
 FROM revisions r JOIN texts t ON t.hash=r.text_hash ORDER BY r.plan_id,r.number;
INSERT INTO board_text(board_text,rank) VALUES('integrity-check',1);
"#;
