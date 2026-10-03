//! Durable board schema and forward-only migration SQL.

pub(super) const SCHEMA_VERSION: i64 = 2;

pub(super) const SCHEMA_V1: &str = r#"
CREATE TABLE board_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE actors(
 id INTEGER PRIMARY KEY, user TEXT NOT NULL, host TEXT NOT NULL,
 harness TEXT NOT NULL, session TEXT NOT NULL, UNIQUE(user,host,harness,session)
);
CREATE TABLE agent_sessions(
 actor_id INTEGER PRIMARY KEY REFERENCES actors(id), model TEXT, effort TEXT,
 cursor_seq INTEGER, bound_plan INTEGER, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL
);
CREATE TABLE repos(repo_key TEXT PRIMARY KEY, origin_label TEXT);
CREATE TABLE repo_paths(
 repo_key TEXT NOT NULL REFERENCES repos(repo_key), host TEXT NOT NULL,
 common_dir TEXT NOT NULL, scan_error TEXT, tips_digest TEXT,
 PRIMARY KEY(repo_key,host,common_dir)
);
CREATE TABLE texts(hash TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE plans(
 id INTEGER PRIMARY KEY, title TEXT NOT NULL, owner_user TEXT NOT NULL,
 steward TEXT, head_revision INTEGER NOT NULL CHECK(head_revision>0),
 next_task INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL
);
CREATE TABLE plan_repos(
 plan_id INTEGER NOT NULL REFERENCES plans(id), repo_key TEXT NOT NULL REFERENCES repos(repo_key),
 PRIMARY KEY(plan_id,repo_key)
);
CREATE TABLE entries(
 id INTEGER PRIMARY KEY, plan_id INTEGER REFERENCES plans(id), kind TEXT NOT NULL,
 body TEXT NOT NULL, to_whom TEXT, supersedes INTEGER REFERENCES entries(id),
 actor_id INTEGER NOT NULL REFERENCES actors(id), model TEXT, effort TEXT,
 repo_key TEXT REFERENCES repos(repo_key), state TEXT, dedupe_key TEXT,
 seq INTEGER NOT NULL, created_at INTEGER NOT NULL
);
CREATE INDEX entries_plan_sequence ON entries(plan_id,seq);
CREATE INDEX entries_dedupe ON entries(actor_id,dedupe_key,created_at);
CREATE TABLE entry_refs(
 entry_id INTEGER NOT NULL REFERENCES entries(id), target TEXT NOT NULL,
 PRIMARY KEY(entry_id,target)
);
CREATE INDEX entry_refs_target ON entry_refs(target);
CREATE TABLE revisions(
 plan_id INTEGER NOT NULL REFERENCES plans(id), number INTEGER NOT NULL CHECK(number>0),
 text_hash TEXT NOT NULL REFERENCES texts(hash), source TEXT NOT NULL CHECK(source IN ('create','accept','direct')),
 entry_id INTEGER REFERENCES entries(id), actor_id INTEGER NOT NULL REFERENCES actors(id),
 seq INTEGER NOT NULL, PRIMARY KEY(plan_id,number)
);
CREATE TABLE proposals(
 entry_id INTEGER PRIMARY KEY REFERENCES entries(id), plan_id INTEGER NOT NULL REFERENCES plans(id),
 base_revision INTEGER NOT NULL, text_hash TEXT NOT NULL REFERENCES texts(hash),
 state TEXT NOT NULL CHECK(state IN ('open','accepted','rejected')),
 decision_entry INTEGER REFERENCES entries(id), result_revision INTEGER,
 FOREIGN KEY(plan_id,base_revision) REFERENCES revisions(plan_id,number)
);
CREATE TABLE tasks(
 plan_id INTEGER NOT NULL REFERENCES plans(id), ordinal INTEGER NOT NULL CHECK(ordinal>0),
 title TEXT NOT NULL, column_name TEXT NOT NULL CHECK(column_name IN ('todo','doing','review','done','blocked')),
 assignee TEXT, section TEXT, seq INTEGER NOT NULL, PRIMARY KEY(plan_id,ordinal)
);
CREATE TABLE claims(
 id INTEGER PRIMARY KEY, plan_id INTEGER NOT NULL, task_ordinal INTEGER NOT NULL,
 actor_id INTEGER NOT NULL REFERENCES actors(id), entry_id INTEGER NOT NULL REFERENCES entries(id),
 scope TEXT NOT NULL, claimed_at INTEGER NOT NULL, last_active INTEGER NOT NULL,
 ended_at INTEGER, end_reason TEXT CHECK(end_reason IN ('released','taken_over','reassigned')),
 FOREIGN KEY(plan_id,task_ordinal) REFERENCES tasks(plan_id,ordinal)
);
CREATE UNIQUE INDEX claims_one_active ON claims(plan_id,task_ordinal) WHERE ended_at IS NULL;
CREATE INDEX claims_actor_active ON claims(actor_id,ended_at);
CREATE TABLE commits(
 repo_key TEXT NOT NULL REFERENCES repos(repo_key), oid TEXT NOT NULL, subject TEXT NOT NULL,
 committed_at INTEGER NOT NULL, author TEXT NOT NULL, coauthors TEXT NOT NULL,
 files INTEGER NOT NULL, file_stats TEXT NOT NULL, insertions INTEGER NOT NULL, deletions INTEGER NOT NULL,
 PRIMARY KEY(repo_key,oid)
);
CREATE TABLE commit_plans(
 repo_key TEXT NOT NULL, oid TEXT NOT NULL, plan_id INTEGER NOT NULL REFERENCES plans(id),
 task_ordinal INTEGER, entry_id INTEGER NOT NULL REFERENCES entries(id),
 PRIMARY KEY(repo_key,oid,plan_id), FOREIGN KEY(repo_key,oid) REFERENCES commits(repo_key,oid),
 FOREIGN KEY(plan_id,task_ordinal) REFERENCES tasks(plan_id,ordinal)
);
CREATE TABLE board_feedback(
 entry_id INTEGER PRIMARY KEY REFERENCES entries(id), feedback_kind TEXT NOT NULL,
 version TEXT NOT NULL, build_id TEXT, cwd TEXT NOT NULL, steer_mode TEXT,
 recent_calls_json TEXT NOT NULL, import_key TEXT UNIQUE
);
CREATE TABLE feedback_imports(
 import_key TEXT PRIMARY KEY, entry_id INTEGER NOT NULL REFERENCES entries(id)
);
CREATE TABLE events(
 seq INTEGER PRIMARY KEY, plan_id INTEGER REFERENCES plans(id), kind TEXT NOT NULL,
 subject TEXT NOT NULL, to_whom TEXT, actor_id INTEGER NOT NULL REFERENCES actors(id),
 summary TEXT NOT NULL, created_at INTEGER NOT NULL
);
CREATE INDEX events_plan_sequence ON events(plan_id,seq);
CREATE INDEX events_recipient_sequence ON events(to_whom,seq);
CREATE TABLE operation_dedupes(
 dedupe_key TEXT PRIMARY KEY, reply_json TEXT NOT NULL, created_at INTEGER NOT NULL
);
"#;

pub(super) const SCHEMA_V2: &str = r#"
UPDATE operation_dedupes SET reply_json=json_set(reply_json,'$.api',2) WHERE json_valid(reply_json) AND json_extract(reply_json,'$.api')=1;
ALTER TABLE agent_sessions DROP COLUMN bound_plan;
DROP INDEX entries_dedupe;
ALTER TABLE entries DROP COLUMN dedupe_key;
ALTER TABLE entries ADD COLUMN via TEXT CHECK(via IS NULL OR via='outbox');
UPDATE entries SET via='outbox' WHERE id IN (
 SELECT entry_id FROM board_feedback WHERE import_key IS NOT NULL
 UNION SELECT entry_id FROM feedback_imports
);
INSERT OR IGNORE INTO plan_repos(plan_id,repo_key)
 SELECT linked.plan_id,canonical.repo_key FROM repo_paths alias
 JOIN plan_repos linked ON linked.repo_key=alias.repo_key
 JOIN repo_paths canonical ON canonical.rowid=(
  SELECT min(first.rowid) FROM repo_paths first WHERE first.host=alias.host AND first.common_dir=alias.common_dir
 );
DELETE FROM repo_paths WHERE rowid NOT IN (SELECT min(rowid) FROM repo_paths GROUP BY host,common_dir);
CREATE UNIQUE INDEX repo_paths_host_common ON repo_paths(host,common_dir);
ALTER TABLE repo_paths DROP COLUMN tips_digest;
ALTER TABLE repo_paths ADD COLUMN registration_error TEXT;
ALTER TABLE repo_paths ADD COLUMN root_commits_json TEXT NOT NULL DEFAULT '[]';
CREATE INDEX entries_sequence ON entries(seq,id);
ALTER TABLE events ADD COLUMN model TEXT;
ALTER TABLE events ADD COLUMN effort TEXT;
UPDATE events SET model=(SELECT model FROM entries WHERE seq=events.seq AND actor_id=events.actor_id ORDER BY id LIMIT 1),
 effort=(SELECT effort FROM entries WHERE seq=events.seq AND actor_id=events.actor_id ORDER BY id LIMIT 1);
DROP INDEX events_recipient_sequence;
CREATE INDEX entries_supersedes ON entries(supersedes);
CREATE INDEX entries_kind_state ON entries(kind,state,seq);
CREATE TABLE proposals_v2(
 entry_id INTEGER PRIMARY KEY REFERENCES entries(id), plan_id INTEGER NOT NULL REFERENCES plans(id),
 base_revision INTEGER NOT NULL, text_hash TEXT NOT NULL REFERENCES texts(hash),
 state TEXT NOT NULL CHECK(state IN ('open','accepted','rejected','superseded')),
 decision_entry INTEGER REFERENCES entries(id), result_revision INTEGER,
 FOREIGN KEY(plan_id,base_revision) REFERENCES revisions(plan_id,number)
);
INSERT INTO proposals_v2 SELECT * FROM proposals;
DROP TABLE proposals;
ALTER TABLE proposals_v2 RENAME TO proposals;
ALTER TABLE commits DROP COLUMN file_stats;
CREATE TABLE commit_tasks(
 repo_key TEXT NOT NULL, oid TEXT NOT NULL, plan_id INTEGER NOT NULL, task_ordinal INTEGER NOT NULL,
 PRIMARY KEY(repo_key,oid,plan_id,task_ordinal),
 FOREIGN KEY(repo_key,oid) REFERENCES commits(repo_key,oid),
 FOREIGN KEY(plan_id,task_ordinal) REFERENCES tasks(plan_id,ordinal)
);
INSERT INTO commit_tasks SELECT repo_key,oid,plan_id,task_ordinal FROM commit_plans WHERE task_ordinal IS NOT NULL;
CREATE TABLE commit_plans_v2(
 repo_key TEXT NOT NULL, oid TEXT NOT NULL, plan_id INTEGER NOT NULL REFERENCES plans(id),
 entry_id INTEGER NOT NULL REFERENCES entries(id), PRIMARY KEY(repo_key,oid,plan_id),
 FOREIGN KEY(repo_key,oid) REFERENCES commits(repo_key,oid)
);
INSERT INTO commit_plans_v2 SELECT repo_key,oid,plan_id,entry_id FROM commit_plans;
DROP TABLE commit_plans;
ALTER TABLE commit_plans_v2 RENAME TO commit_plans;
CREATE INDEX commit_plans_plan ON commit_plans(plan_id);
CREATE INDEX commit_plans_entry ON commit_plans(entry_id);
CREATE TABLE claims_v2(
 id INTEGER PRIMARY KEY, plan_id INTEGER NOT NULL, task_ordinal INTEGER NOT NULL,
 actor_id INTEGER NOT NULL REFERENCES actors(id), entry_id INTEGER NOT NULL REFERENCES entries(id),
 scope TEXT NOT NULL, claimed_at INTEGER NOT NULL, last_active INTEGER NOT NULL,
 ended_at INTEGER, end_reason TEXT CHECK(end_reason IN ('released','taken_over','reassigned','resumed')),
 FOREIGN KEY(plan_id,task_ordinal) REFERENCES tasks(plan_id,ordinal)
);
INSERT INTO claims_v2 SELECT * FROM claims;
DROP TABLE claims;
ALTER TABLE claims_v2 RENAME TO claims;
CREATE UNIQUE INDEX claims_one_active ON claims(plan_id,task_ordinal) WHERE ended_at IS NULL;
CREATE INDEX claims_actor_active ON claims(actor_id,ended_at);
CREATE INDEX claims_entry_active ON claims(entry_id,id) WHERE ended_at IS NULL;
"#;
