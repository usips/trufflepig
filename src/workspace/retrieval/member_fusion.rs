//! Member answers become coverage rows and quota-bounded ranked lists that
//! interleave round-robin by member rank; raw scores are never compared.
use super::{
    member_coverage::{member_row, searched_row, short_reason},
    member_scope::implicit_home_scope,
    member_search::{FoundMember, MemberAnswer, SearchedMember},
    timeout_without_member_answer,
};
use crate::{
    coded_error::{CodeDetail, CodedError, ErrorCode},
    results::ResultEntry,
    search,
    store::{Store, is_index_warming},
    workspace::{
        WorkspaceConfig,
        member_root::MemberRoot,
        result_cache::{OwnedEntry, WorkspaceSet},
    },
};
use anyhow::{Result, ensure};

/// Workspace results as members answer, before interleaving.
pub(super) struct MemberFusion {
    set: WorkspaceSet,
    lists: Vec<std::vec::IntoIter<OwnedEntry>>,
    /// Per owner: its coverage row and, when reranking, its answering store.
    owner_rows: Vec<usize>,
    stores: Vec<Option<Store>>,
    /// Where the first timed-out member's deadline expired.
    first_timeout_phase: Option<&'static str>,
}

/// The interleaved result set with what the merged rerank needs per owner.
pub(super) struct FusedMembers {
    pub set: WorkspaceSet,
    /// `set.coverage` index of each owner's row.
    pub owner_rows: Vec<usize>,
    /// Each owner's answering store, present only when reranking.
    pub stores: Vec<Option<Store>>,
}

impl MemberFusion {
    pub fn new(config: &WorkspaceConfig, roots: &[MemberRoot]) -> Self {
        let home = roots.iter().find(|root| root.is_home);
        Self {
            set: WorkspaceSet {
                workspace: config.name.clone(),
                home: home.map(|root| root.name().to_owned()),
                owners: Vec::with_capacity(roots.len()),
                coverage: Vec::with_capacity(roots.len()),
                hits: Vec::new(),
                truncated: false,
                scope: None,
            },
            lists: Vec::with_capacity(roots.len()),
            owner_rows: Vec::with_capacity(roots.len()),
            stores: Vec::with_capacity(roots.len()),
            first_timeout_phase: None,
        }
    }

    /// Adds one member's answer under an equal share of the staging capacity
    /// among `members` queued members; returns the member's coverage state.
    pub fn admit(
        &mut self,
        member: &MemberRoot,
        searched: SearchedMember,
        members: usize,
    ) -> Result<&'static str> {
        let SearchedMember {
            answer,
            progress_source,
        } = searched;
        let (mut row, state) = match answer {
            Ok(MemberAnswer::Found(answer)) => {
                (self.admit_found(member, *answer, members)?, "searched")
            }
            Ok(MemberAnswer::Warming(reason)) => {
                let mut row = member_row(member, "warming");
                if let Some(reason) = reason {
                    row["reason"] = reason.into();
                }
                (row, "warming")
            }
            Err(error) => {
                let state = if is_index_warming(&error) {
                    "warming"
                } else if ErrorCode::TimedOut.is_code_of(&error) {
                    "timed_out"
                } else {
                    "unavailable"
                };
                if state == "timed_out" && self.first_timeout_phase.is_none() {
                    let recheck = CodeDetail::CurrentSourceRecheck;
                    self.first_timeout_phase = Some(if recheck.is_anywhere_in(&error) {
                        "current_source_recheck"
                    } else {
                        "member_processing"
                    });
                }
                let mut row = member_row(member, state);
                if matches!(state, "unavailable" | "timed_out") {
                    row["reason"] = short_reason(&error).into();
                }
                (row, state)
            }
        };
        if let Some(source) = &progress_source {
            row["index_progress_source"] = serde_json::to_value(source)?;
        }
        self.set.coverage.push(row);
        Ok(state)
    }

    fn admit_found(
        &mut self,
        member: &MemberRoot,
        answer: FoundMember,
        members: usize,
    ) -> Result<serde_json::Value> {
        let FoundMember {
            mut owner,
            found,
            fallback,
            store,
        } = answer;
        // A member cannot reserve the full workspace staging capacity before others run.
        let byte_quota = crate::results::MAX_BYTES / members;
        let hit_quota = crate::results::MAX_HITS / members;
        owner.coverage = found.coverage.clone();
        let owner_index = self.set.owners.len();
        let mut bytes = 0;
        let mut entries = Vec::with_capacity(found.hits.len().min(hit_quota));
        let total = found.hits.len();
        for (rank, hit) in found.hits.into_iter().enumerate().take(hit_quota) {
            let entry = OwnedEntry {
                owner: owner_index,
                member_rank: rank + 1,
                worktree_differs: fallback.as_ref().is_some_and(|(_, d)| d.flags(&hit)),
                entry: ResultEntry::LiveSource(hit),
            };
            bytes += serde_json::to_vec(&entry)?.len();
            if bytes > byte_quota {
                break;
            }
            entries.push(entry);
        }
        let truncated = found.truncated || entries.len() < total;
        let mut row = searched_row(
            member,
            owner.generation,
            entries.len(),
            truncated,
            &found.coverage,
        );
        if let Some(detail) = found.coverage.get("worktree_reextraction") {
            describe_worktree_reextraction(&mut row, detail);
        }
        if let Some((fallback, differing_files)) = &fallback {
            let differing = entries.iter().filter(|e| e.worktree_differs).count();
            fallback.describe(&mut row, differing_files, differing);
        }
        for key in search::REFERENCE_COVERAGE_KEYS {
            if let Some(value) = found.coverage.get(key) {
                row[key] = value.clone();
            }
        }
        self.set.truncated |= truncated;
        self.set.owners.push(owner);
        self.owner_rows.push(self.set.coverage.len());
        self.stores.push(store);
        self.lists.push(entries.into_iter());
        Ok(row)
    }

    /// Records the implicit home scope after home answered; returns whether
    /// the query widens to the other members.
    pub fn settle_home_scope(&mut self, other_members: usize, home_state: &str) -> bool {
        let home_hits = self.lists.first().is_some_and(|list| list.len() > 0);
        let incomplete_fallback =
            self.set.coverage.last().is_some_and(|row| {
                row["issues"]["worktree_reextraction"]["status"] == "incomplete"
            });
        let home_answered = !self.lists.is_empty() && !incomplete_fallback;
        let home_state = if incomplete_fallback {
            "incomplete"
        } else {
            home_state
        };
        let (scope, widen) =
            implicit_home_scope(other_members, home_answered, home_hits, home_state);
        self.set.scope = Some(scope);
        widen
    }

    /// Fails an unanswerable query, then interleaves member ranks round-robin.
    pub fn interleave(
        mut self,
        map_miss: Option<CodedError>,
        implicit_home: bool,
    ) -> Result<FusedMembers> {
        if self.lists.iter().all(|list| list.len() == 0)
            && let Some(miss) = map_miss
        {
            anyhow::bail!(miss);
        }
        if let Some(error) = timeout_without_member_answer(
            self.set.owners.len(),
            &self.set.coverage,
            self.first_timeout_phase,
        ) {
            return Err(error);
        }
        // An unselected query whose home is not answering reports that on an empty page.
        ensure!(
            !self.set.owners.is_empty() || implicit_home,
            coded!(
                WorkspaceUnavailable,
                "no selected member has an available published index; inspect ws status or use --no-daemon"
            )
        );
        self.set
            .hits
            .reserve(self.lists.iter().map(|list| list.len()).sum());
        loop {
            let before = self.set.hits.len();
            for list in &mut self.lists {
                self.set.hits.extend(list.next());
            }
            if self.set.hits.len() == before {
                break;
            }
        }
        Ok(FusedMembers {
            set: self.set,
            owner_rows: self.owner_rows,
            stores: self.stores,
        })
    }
}

/// Marks a parent-fallback row partial and truncated with its re-extraction detail.
fn describe_worktree_reextraction(row: &mut serde_json::Value, detail: &serde_json::Value) {
    row["partial"] = true.into();
    row["truncated"] = true.into();
    if let Some(unsearched) = detail["unsearched_paths"].as_u64()
        && detail["unsearched_paths_exact"] == true
    {
        let known = row["unsearched"].as_u64().unwrap_or(0);
        row["unsearched"] = known.saturating_add(unsearched).into();
        if unsearched > 0 {
            if !row["unsearched_kinds"].is_array() {
                row["unsearched_kinds"] = serde_json::Value::Array(Vec::new());
            }
            let kinds = row["unsearched_kinds"]
                .as_array_mut()
                .expect("array initialized above");
            if !kinds.iter().any(|kind| kind == "worktree_reextraction") {
                kinds.push("worktree_reextraction".into());
            }
        }
    }
    row["issues"]["worktree_reextraction"] = detail.clone();
}
