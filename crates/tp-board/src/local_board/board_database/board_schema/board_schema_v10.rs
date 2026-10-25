//! Atomic API 10 evidence, authority, worker, and progress schema.

pub(in crate::local_board::board_database) const SCHEMA_V10: &str = r#"
ALTER TABLE entries ADD COLUMN origin_harness TEXT;
ALTER TABLE entries ADD COLUMN origin_session TEXT;
ALTER TABLE entries ADD COLUMN authority_entry INTEGER REFERENCES entries(id);
ALTER TABLE entries ADD COLUMN authority_basis TEXT CHECK(
 (authority_basis IS NULL AND authority_entry IS NULL) OR
 (authority_basis IS NOT NULL AND authority_basis IN ('owner','reporter','stale','holder','delegator') AND authority_entry IS NULL) OR
 (authority_basis='grant' AND authority_entry IS NOT NULL)
);
ALTER TABLE events ADD COLUMN origin_harness TEXT;
ALTER TABLE events ADD COLUMN origin_session TEXT;
ALTER TABLE events ADD COLUMN authority_entry INTEGER REFERENCES entries(id);
ALTER TABLE events ADD COLUMN authority_basis TEXT CHECK(
 (authority_basis IS NULL AND authority_entry IS NULL) OR
 (authority_basis IS NOT NULL AND authority_basis IN ('owner','reporter','stale','holder','delegator') AND authority_entry IS NULL) OR
 (authority_basis='grant' AND authority_entry IS NOT NULL)
);
ALTER TABLE agent_sessions ADD COLUMN ended_at INTEGER;

CREATE TABLE claims_v10(
 id INTEGER PRIMARY KEY, plan_id INTEGER NOT NULL, task_ordinal INTEGER NOT NULL,
 actor_id INTEGER NOT NULL REFERENCES actors(id), entry_id INTEGER NOT NULL REFERENCES entries(id),
 scope TEXT NOT NULL, claimed_at INTEGER NOT NULL, last_active INTEGER NOT NULL,
 ended_at INTEGER,
 end_reason TEXT CHECK(end_reason IN ('released','taken_over','reassigned','resumed','session_ended')),
 delegated_by INTEGER REFERENCES actors(id),
 FOREIGN KEY(plan_id,task_ordinal) REFERENCES tasks(plan_id,ordinal)
);
INSERT INTO claims_v10 SELECT id,plan_id,task_ordinal,actor_id,entry_id,scope,claimed_at,
 last_active,ended_at,end_reason,delegated_by FROM claims;
DROP TABLE claims;
ALTER TABLE claims_v10 RENAME TO claims;
CREATE UNIQUE INDEX claims_one_active ON claims(plan_id,task_ordinal) WHERE ended_at IS NULL;
CREATE INDEX claims_actor_active ON claims(actor_id,ended_at);
CREATE INDEX claims_entry_active ON claims(entry_id,id) WHERE ended_at IS NULL;

CREATE TABLE authority_grants(
 entry_id INTEGER PRIMARY KEY REFERENCES entries(id),
 plan_id INTEGER REFERENCES plans(id),
 owner_user TEXT NOT NULL,
 roles_json TEXT NOT NULL CHECK(json_valid(roles_json) AND json_type(roles_json)='array'),
 grantee TEXT NOT NULL,
 granted_at INTEGER NOT NULL,
 expires_at INTEGER,
 revoked_by_entry INTEGER REFERENCES entries(id),
 source TEXT NOT NULL CHECK(source IN ('migration','owner'))
);
CREATE INDEX authority_grants_active
 ON authority_grants(plan_id,owner_user,grantee,expires_at)
 WHERE revoked_by_entry IS NULL;

INSERT OR IGNORE INTO actors(user,host,harness,session)
 SELECT 'system','board-v10','system','migration'
 WHERE EXISTS(SELECT 1 FROM plans WHERE steward IS NOT NULL AND steward<>'');
CREATE TEMP TABLE board_v10_legacy_stewards(
 plan_id INTEGER PRIMARY KEY,
 entry_id INTEGER NOT NULL,
 seq INTEGER NOT NULL,
 owner_user TEXT NOT NULL,
 grantee TEXT NOT NULL,
 granted_at INTEGER NOT NULL
);
WITH numbered AS (
 SELECT id,owner_user,steward,ROW_NUMBER() OVER (ORDER BY id) AS ordinal
 FROM plans WHERE steward IS NOT NULL AND steward<>''
), maxima AS (
 SELECT COALESCE((SELECT MAX(id) FROM entries),0) AS entry_id,
        max(COALESCE((SELECT MAX(seq) FROM entries),0),
            COALESCE((SELECT MAX(seq) FROM events),0)) AS seq
)
INSERT INTO board_v10_legacy_stewards(plan_id,entry_id,seq,owner_user,grantee,granted_at)
 SELECT numbered.id,maxima.entry_id+numbered.ordinal,maxima.seq+numbered.ordinal,
        numbered.owner_user,numbered.steward,CAST(strftime('%s','now') AS INTEGER)
 FROM numbered CROSS JOIN maxima;
INSERT INTO entries(id,plan_id,kind,body,to_whom,actor_id,seq,created_at,authority_basis)
 SELECT legacy.entry_id,legacy.plan_id,'grant',
        'System migration: legacy steward grant for P'||legacy.plan_id||
        ' to harness '||legacy.grantee||' under owner '||legacy.owner_user,
        legacy.grantee,
        (SELECT id FROM actors WHERE user='system' AND host='board-v10' AND harness='system' AND session='migration'),
        legacy.seq,legacy.granted_at,'owner'
 FROM board_v10_legacy_stewards AS legacy;
INSERT INTO events(seq,plan_id,kind,subject,to_whom,actor_id,summary,created_at,authority_basis)
 SELECT legacy.seq,legacy.plan_id,'grant','E'||legacy.entry_id,legacy.grantee,
        (SELECT id FROM actors WHERE user='system' AND host='board-v10' AND harness='system' AND session='migration'),
        'System migration: legacy steward grant for P'||legacy.plan_id||
        ' to harness '||legacy.grantee||' under owner '||legacy.owner_user,
        legacy.granted_at,'owner'
 FROM board_v10_legacy_stewards AS legacy;
INSERT INTO authority_grants(entry_id,plan_id,owner_user,roles_json,grantee,granted_at,source)
 SELECT entry_id,plan_id,owner_user,
        '["triage","release","link","assign","decide"]',grantee,granted_at,'migration'
 FROM board_v10_legacy_stewards;
DROP TABLE board_v10_legacy_stewards;
ALTER TABLE plans DROP COLUMN steward;

CREATE TABLE claim_workers(
 id INTEGER PRIMARY KEY,
 claim_id INTEGER NOT NULL REFERENCES claims(id),
 entry_id INTEGER NOT NULL UNIQUE REFERENCES entries(id),
 plan_id INTEGER NOT NULL,
 task_ordinal INTEGER NOT NULL,
 handle TEXT NOT NULL CHECK(length(handle) BETWEEN 1 AND 32 AND handle=lower(handle)),
 actor_id INTEGER REFERENCES actors(id),
 model TEXT NOT NULL,
 effort TEXT,
 role TEXT NOT NULL CHECK(role IN ('code','review','test','research')),
 scope TEXT NOT NULL,
 files_json TEXT NOT NULL CHECK(json_valid(files_json) AND json_type(files_json)='array'),
 added_at INTEGER NOT NULL,
 last_active INTEGER NOT NULL,
 ended_at INTEGER,
 end_reason TEXT CHECK(end_reason IS NULL OR end_reason IN ('done','failed','stopped','claim_ended')),
 CHECK((ended_at IS NULL AND end_reason IS NULL) OR
       (ended_at IS NOT NULL AND end_reason IS NOT NULL)),
 FOREIGN KEY(plan_id,task_ordinal) REFERENCES tasks(plan_id,ordinal),
 UNIQUE(plan_id,task_ordinal,handle)
);
CREATE INDEX claim_workers_claim_active ON claim_workers(claim_id,ended_at,id);
CREATE UNIQUE INDEX claim_workers_actor_active ON claim_workers(actor_id)
 WHERE actor_id IS NOT NULL AND ended_at IS NULL;
ALTER TABLE entries ADD COLUMN worker_id INTEGER REFERENCES claim_workers(id);

CREATE TABLE review_verdicts(
 entry_id INTEGER PRIMARY KEY REFERENCES entries(id),
 verdict TEXT NOT NULL CHECK(verdict IN ('clean','findings','blocking')),
 count INTEGER,
 CHECK((verdict='clean' AND count IS NULL) OR
       (verdict IN ('findings','blocking') AND count IS NOT NULL AND count>=0))
);
CREATE TABLE progress_facts(
 entry_id INTEGER PRIMARY KEY REFERENCES entries(id),
 claim_id INTEGER NOT NULL REFERENCES claims(id),
 worker_id INTEGER REFERENCES claim_workers(id),
 head_oid TEXT,
 head_verified INTEGER CHECK(head_verified IS NULL OR head_verified IN (0,1)),
 step_done INTEGER,
 step_total INTEGER,
 tests_passed INTEGER,
 tests_failed INTEGER,
 eta_at INTEGER,
 updated_at INTEGER NOT NULL,
 CHECK((head_oid IS NULL AND head_verified IS NULL) OR
       (head_oid IS NOT NULL AND head_verified IS NOT NULL)),
 CHECK((step_done IS NULL AND step_total IS NULL) OR
       (step_done IS NOT NULL AND step_total IS NOT NULL AND
        step_done>=0 AND step_total>=0 AND step_done<=step_total)),
 CHECK((tests_passed IS NULL AND tests_failed IS NULL) OR
       (tests_passed IS NOT NULL AND tests_failed IS NOT NULL AND
        tests_passed>=0 AND tests_failed>=0))
);
CREATE INDEX progress_facts_claim_time ON progress_facts(claim_id,updated_at,entry_id);
CREATE INDEX progress_facts_worker_time ON progress_facts(worker_id,updated_at,entry_id)
 WHERE worker_id IS NOT NULL;
CREATE TABLE progress_gates(
 entry_id INTEGER NOT NULL REFERENCES progress_facts(entry_id),
 name TEXT NOT NULL,
 result TEXT NOT NULL CHECK(result IN ('pass','fail','running')),
 PRIMARY KEY(entry_id,name)
);

ALTER TABLE commits ADD COLUMN worker_refs_json TEXT NOT NULL DEFAULT '[]'
 CHECK(json_valid(worker_refs_json) AND json_type(worker_refs_json)='array');
ALTER TABLE commit_tasks ADD COLUMN claim_entry INTEGER REFERENCES entries(id);
ALTER TABLE commit_tasks ADD COLUMN attribution_basis TEXT
 CHECK((attribution_basis IS NULL AND claim_entry IS NULL) OR
       (attribution_basis IS NOT NULL AND attribution_basis='claim_window' AND claim_entry IS NOT NULL));
ALTER TABLE commit_tasks ADD COLUMN claim_actor_id INTEGER REFERENCES actors(id);
CREATE INDEX commit_tasks_claim_entry ON commit_tasks(claim_entry)
 WHERE claim_entry IS NOT NULL;

DELETE FROM operation_dedupes;
"#;
