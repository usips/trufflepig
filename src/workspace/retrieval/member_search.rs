//! One member's answer from its own published, parent, or inline-built index.
//! Member searches never rerank; [`super::fused_rerank`] scores the merged
//! window once, so a member answer keeps its store for that stage.
use super::{WorkspaceQuery, inline_member_index::inline_member_index};
use crate::{
    coded_error::{CodedContext, CodedError, ErrorCode},
    diagnostics::{DiagnosticsMode, EventStage, Operation, Outcome, RequestEvent},
    results::ResultSet,
    search::{self, Query, telemetry::RetrievalTrace},
    semantic::{Embedding, SemanticSession},
    store::Store,
    workspace::{
        coordinator, hashed_member_cache,
        home_index::{
            DifferingFiles, HomeIndexPolicy, HomeIndexSource, ParentFallback, WorktreeHashes,
            resolve_home_index,
        },
        member_cache_until,
        member_root::MemberRoot,
        result_cache::{IndexProgressSource, MemberSnapshot},
    },
};
use anyhow::{Result, ensure};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

/// The parsed command every member runs, with its query vector prepared once.
pub(super) struct MemberCommand<'w> {
    pub verb: &'w str,
    pub words: &'w [String],
    pub text: String,
    pub query: Query,
    pub semantic: bool,
    /// The merged window is reranked once after fusion.
    pub rerank: bool,
    pub mode: DiagnosticsMode,
    /// Workspace diagnostics location; members fall back to their own caches.
    pub log_cache: Option<PathBuf>,
    prepared: Option<Embedding>,
    semantic_error: Option<String>,
    preparation_us: u64,
    invocation_root: PathBuf,
}

impl<'w> MemberCommand<'w> {
    pub fn prepare(
        request: &WorkspaceQuery,
        words: &'w [String],
        invocation_root: PathBuf,
        session: &mut SemanticSession,
    ) -> Result<Self> {
        let options = request.options;
        let log_cache = crate::workspace::diagnostic_location(options)
            .ok()
            .map(|(_, cache, _, _)| cache);
        let verb = words
            .first()
            .map(String::as_str)
            .with_coded_context(|| coded!(Usage, "retrieval command is required"))?;
        let text = words[1..].join(" ");
        let query = Query::parse(&text)?;
        let semantic_eligible_shape = query.semantic_eligible()
            && !matches!(verb, "refs" | "map")
            && !text.starts_with("refs:");
        let semantic = options.sem && semantic_eligible_shape;
        let started = Instant::now();
        session.set_no_daemon(options.no_daemon);
        let (prepared, semantic_error) = match session.prepare(semantic, request.cache, &query.text)
        {
            Ok(prepared) => (prepared, None),
            // Keep the cause chain: the outer context alone hides why the worker failed.
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let mode = match options.diagnostics.as_str() {
            "off" => DiagnosticsMode::Off,
            "detailed" => DiagnosticsMode::Detailed,
            _ => DiagnosticsMode::Metadata,
        };
        Ok(Self {
            verb,
            words,
            text,
            query,
            semantic,
            rerank: options.rerank && semantic_eligible_shape,
            mode,
            log_cache,
            prepared,
            semantic_error,
            preparation_us: started.elapsed().as_micros().min(u64::MAX as u128) as u64,
            invocation_root,
        })
    }

    fn references(&self) -> bool {
        self.verb == "refs" || self.text.starts_with("refs:")
    }

    pub fn trace(&self) -> RetrievalTrace {
        if matches!(self.mode, DiagnosticsMode::Off) {
            RetrievalTrace::disabled()
        } else {
            RetrievalTrace::default()
        }
    }
}

/// One member's hits, or warming without a published index.
pub(super) enum MemberAnswer {
    Found(Box<FoundMember>),
    Warming(Option<&'static str>),
}

/// A member's ranked answer and the snapshot that owns its hits.
pub(super) struct FoundMember {
    pub owner: MemberSnapshot,
    pub found: ResultSet,
    /// A parent-index answer and the files among its hits that differ.
    pub fallback: Option<(ParentFallback, DifferingFiles)>,
    /// The answering index, kept only for the merged rerank's body reads.
    pub store: Option<Store>,
}

/// A member search outcome with the index progress source it observed.
pub(super) struct SearchedMember {
    pub answer: Result<MemberAnswer>,
    pub progress_source: Option<IndexProgressSource>,
}

/// Searches one member and records its server diagnostics event; the first
/// member's event also carries the shared query preparation time.
pub(super) fn search_member(
    request: &WorkspaceQuery,
    command: &MemberCommand,
    member: &MemberRoot,
    first: bool,
    map_miss: &mut Option<CodedError>,
) -> Result<SearchedMember> {
    let hashed_cache = hashed_member_cache(&member.root, request.options.cache.as_deref())?;
    let started = Instant::now();
    let mut trace = command.trace();
    let mut progress_source = None;
    let search = MemberSearch {
        request,
        command,
        member,
        hashed_cache: &hashed_cache,
    };
    let answer = search
        .answer(&mut trace, &mut progress_source, map_miss)
        .map_err(|error| request.deadline.classify(error));
    if command.semantic && first {
        trace.query_preparation_us = Some(command.preparation_us);
    }
    let outcome = if matches!(answer, Ok(MemberAnswer::Found(_))) {
        Outcome::Success
    } else {
        Outcome::Unavailable
    };
    let mut event = RequestEvent::new(request.context.clone(), Operation::Search, outcome);
    event.stage = EventStage::Server;
    event.elapsed_micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
    event.retrieval = Some(trace);
    event.workspace = Some(request.config.id.clone());
    event.member = Some(member.name().to_owned());
    let log_cache = command.log_cache.as_deref().unwrap_or(&hashed_cache);
    crate::diagnostics::best_effort_record(log_cache, command.mode, event);
    Ok(SearchedMember {
        answer,
        progress_source,
    })
}

/// One member's search inputs; `hashed_cache` is the member's diagnostics
/// cache and its cache when an inline maintenance slice times out.
struct MemberSearch<'a> {
    request: &'a WorkspaceQuery<'a>,
    command: &'a MemberCommand<'a>,
    member: &'a MemberRoot,
    hashed_cache: &'a Path,
}

impl MemberSearch<'_> {
    fn answer(
        &self,
        trace: &mut RetrievalTrace,
        progress_source: &mut Option<IndexProgressSource>,
        map_miss: &mut Option<CodedError>,
    ) -> Result<MemberAnswer> {
        let Self {
            request,
            command,
            member,
            ..
        } = *self;
        let (options, deadline) = (request.options, &request.deadline);
        let explicit_cache = options.cache.as_deref();
        ensure!(
            !deadline.expired(),
            coded!(TimedOut, "query deadline expired")
        );
        member.verify_identity()?;
        let inline = matches!(request.policy, HomeIndexPolicy::IndexInline);
        let inline_deadline = inline.then(|| crate::store::inline_maintenance_deadline(deadline));
        let cache_deadline = inline_deadline.as_ref().unwrap_or(deadline);
        let member_cache = match member_cache_until(member, explicit_cache, cache_deadline) {
            Ok(cache) => cache,
            Err(error)
                if inline_deadline
                    .as_ref()
                    .is_some_and(|slice| !deadline.expired() && slice.expired())
                    && ErrorCode::TimedOut.is_code_of(&error) =>
            {
                self.hashed_cache.to_owned()
            }
            Err(error) => return Err(error),
        };
        *progress_source = IndexProgressSource::capture(&member.root, &member_cache);
        coordinator::ensure_member(member, options, deadline)?;
        let source = match inline_deadline {
            Some(slice) => {
                inline_member_index(member, &member_cache, explicit_cache, deadline, slice)?
            }
            None => resolve_home_index(
                member,
                &member_cache,
                explicit_cache,
                request.policy,
                deadline.clone(),
            ),
        };
        let (store, fallback) = match source {
            HomeIndexSource::Own(store) => (store, None),
            HomeIndexSource::Parent(view) => (view.store, Some(view.fallback)),
            HomeIndexSource::Warming { reason } => return Ok(MemberAnswer::Warming(reason)),
            HomeIndexSource::Unavailable { error } => return Err(error),
        };
        let preparation_error = if command.semantic && !options.no_daemon {
            crate::semantic_preparation::schedule(&member.root, &member_cache).err()
        } else {
            None
        };
        let origin = search::InvocationDirectory::within(&member.root, &command.invocation_root);
        let mut found = self.run_command(&store, fallback.is_none(), &origin, trace)?;
        if command.verb == "map"
            && let Some(miss) = search::map_miss(&found)
        {
            map_miss.get_or_insert(miss);
        }
        if let Some(error) = &command.semantic_error {
            found.coverage["semantic_status"] = "unavailable".into();
            found.coverage["semantic_reason"] = error.clone().into();
        }
        if let Some(error) = preparation_error {
            found.coverage["semantic_preparation_error"] = error.to_string().into();
        }
        let fallback = match fallback {
            Some(mut fallback) => {
                let mut hashes = WorktreeHashes::default();
                let call = (command.verb, command.words, &command.query, &origin);
                let changed =
                    fallback.complete_answer(&store, call, &mut found, &mut hashes, deadline)?;
                let differing = fallback.check_hits(&store.root, &found.hits, &mut hashes, changed);
                Some((fallback, differing))
            }
            None => None,
        };
        if found.coverage["truncated_files"].as_u64().unwrap_or(0) > 0 {
            let paths = super::member_coverage::fact_limit_paths(&store)?;
            found.coverage["unsearched_paths"] = paths.into();
        }
        let owner = MemberSnapshot::capture(member, &store, &member_cache, found.generation)?;
        Ok(MemberAnswer::Found(Box::new(FoundMember {
            owner,
            found,
            fallback,
            store: command.rerank.then_some(store),
        })))
    }

    /// Runs `refs`, `map`, or an unreranked search against one member index.
    fn run_command(
        &self,
        store: &Store,
        own_index: bool,
        origin: &search::InvocationDirectory,
        trace: &mut RetrievalTrace,
    ) -> Result<ResultSet> {
        let command = self.command;
        if command.references() {
            return search::references(store, &search::reference_query(&command.text)?);
        }
        if command.verb == "map" {
            let path = command.words.get(1).map(String::as_str).unwrap_or("");
            let mut found = search::map(store, path)?;
            if own_index && self.member.worktree.is_some() {
                ParentFallback::refresh_current_map(store, path, &mut found)?;
            }
            return Ok(found);
        }
        let mut found = search::search_prepared(
            store,
            &command.query,
            store.index_cache(),
            command.prepared.clone(),
            None,
            trace,
            &self.request.deadline,
        )?;
        if command.query.exact {
            // `sym:` namesakes nearest the invocation directory come first.
            search::rank_definitions(&mut found.hits, origin);
        }
        Ok(found)
    }
}
