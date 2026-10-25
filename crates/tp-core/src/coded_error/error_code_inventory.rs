//! The stable error-code vocabulary; see `docs/diagnostics-contract.md#error-codes`.
//! Each code's text is the `code:` prefix agents and tests parse.

macro_rules! error_codes {
    ($($variant:ident => $text:literal,)+) => {
        /// One stable error code; [`ErrorCode::as_str`] is its wire spelling.
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum ErrorCode {
            $($variant,)+
        }

        impl ErrorCode {
            /// Every code, in declaration order.
            #[cfg(test)]
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }

            /// The code spelled exactly `label`, if any.
            pub fn parse(label: &str) -> Option<Self> {
                match label {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

error_codes! {
    BackgroundExecutableUnavailable => "background_executable_unavailable",
    BackgroundSpawnFailed => "background_spawn_failed",
    BoardApiMismatch => "board_api_mismatch",
    BoardInitializationRequired => "board_initialization_required",
    BoardLink => "board_link",
    BoardRemoteUnsupported => "board_remote_unsupported",
    BoardScan => "board_scan",
    BoardServeLock => "board_serve_lock",
    BoardUnavailable => "board_unavailable",
    BoardWebChallenge => "board_web_challenge",
    BoardWebShutdown => "board_web_shutdown",
    BoardWebToken => "board_web_token",
    BudgetTooSmall => "budget_too_small",
    CacheFreeUnsupported => "cache_free_unsupported",
    CacheUnavailable => "cache_unavailable",
    ClaimConflict => "claim_conflict",
    CoordinatorCacheCreateFailed => "coordinator_cache_create_failed",
    CudaUnavailable => "cuda_unavailable",
    CurrentSourceDuplicate => "current_source_duplicate",
    DaemonBusy => "daemon_busy",
    DaemonCacheCreateFailed => "daemon_cache_create_failed",
    DaemonReplyTimeout => "daemon_reply_timeout",
    DaemonUnavailable => "daemon_unavailable",
    DatabaseLocked => "database is locked",
    ExpiredResult => "expired_result",
    ExtractionUnavailable => "extraction_unavailable",
    HandshakeMismatch => "handshake_mismatch",
    HistoricalResult => "historical_result",
    HistoryCacheMismatch => "history_cache_mismatch",
    HistoryCacheUnavailable => "history_cache_unavailable",
    HistoryChanged => "history_changed",
    HistoryRegistrationLimit => "history_registration_limit",
    HistoryResourceLimited => "history_resource_limited",
    HistorySourceExcluded => "history_source_excluded",
    HistoryUnavailable => "history_unavailable",
    IndexCacheUnavailable => "index_cache_unavailable",
    IndexWarming => "index_warming",
    InferenceConfigInvalid => "inference_config_invalid",
    InferenceConfigUnavailable => "inference_config_unavailable",
    InternalError => "internal_error",
    InvalidActor => "invalid_actor",
    InvalidAuthority => "invalid_authority",
    InvalidBody => "invalid_body",
    InvalidBudget => "invalid_budget",
    InvalidCache => "invalid_cache",
    InvalidClient => "invalid_client",
    InvalidCommand => "invalid_command",
    InvalidCursor => "invalid_cursor",
    InvalidDuration => "invalid_duration",
    InvalidEmbedding => "invalid_embedding",
    InvalidHandle => "invalid_handle",
    InvalidKind => "invalid_kind",
    InvalidLimit => "invalid_limit",
    InvalidMember => "invalid_member",
    InvalidMetadata => "invalid_metadata",
    InvalidOid => "invalid_oid",
    InvalidOptions => "invalid_options",
    InvalidOrigin => "invalid_origin",
    InvalidPath => "invalid_path",
    InvalidProgress => "invalid_progress",
    InvalidQuery => "invalid_query",
    InvalidRange => "invalid_range",
    InvalidReference => "invalid_reference",
    InvalidRole => "invalid_role",
    InvalidRevision => "invalid_revision",
    InvalidRoot => "invalid_root",
    InvalidScope => "invalid_scope",
    InvalidSide => "invalid_side",
    InvalidSpan => "invalid_span",
    InvalidState => "invalid_state",
    InvalidTarget => "invalid_target",
    InvalidVerdict => "invalid_verdict",
    InvalidWorker => "invalid_worker",
    InvalidWorkspace => "invalid_workspace",
    MemberRequired => "member_required",
    MemberUnavailable => "member_unavailable",
    NoDefinition => "no_definition",
    NoIndexedPath => "no_indexed_path",
    PreparationBusy => "preparation_busy",
    PreparationCacheMismatch => "preparation_cache_mismatch",
    PreparationUnavailable => "preparation_unavailable",
    PreparationWorkerFailed => "preparation_worker_failed",
    PreparationWorkerTimeout => "preparation_worker_timeout",
    ReadOnlyStore => "read_only_store",
    RerankAdmission => "rerank_admission",
    RerankLoading => "rerank_loading",
    RerankUnavailable => "rerank_unavailable",
    RerankWorker => "rerank_worker",
    ResultCacheUnavailable => "result_cache_unavailable",
    ResultUnavailable => "result_unavailable",
    RouterBinaryReplaced => "router_binary_replaced",
    RouterCacheCreateFailed => "router_cache_create_failed",
    SchemaNewer => "schema_newer",
    ScopeBoundary => "scope_boundary",
    SemanticAdmission => "semantic_admission",
    SemanticBusy => "semantic_busy",
    SemanticCacheLocked => "semantic_cache_locked",
    SemanticCacheUnavailable => "semantic_cache_unavailable",
    SemanticCuda => "semantic_cuda",
    SemanticFallbackBudgetTooSmall => "semantic_fallback_budget_too_small",
    SemanticFallbackOutputInvalid => "semantic_fallback_output_invalid",
    SemanticFallbackOutputMissingCoverage => "semantic_fallback_output_missing_coverage",
    SemanticFallbackOutputTooLarge => "semantic_fallback_output_too_large",
    SemanticFallbackRootReservationFailed => "semantic_fallback_root_reservation_failed",
    SemanticLoading => "semantic_loading",
    SemanticPending => "semantic_pending",
    SemanticRootBusy => "semantic_root_busy",
    SemanticTimeout => "semantic_timeout",
    SemanticUnavailable => "semantic_unavailable",
    SemanticWorker => "semantic_worker",
    SemanticWorkerBusy => "semantic_worker_busy",
    SemanticWorkerUnavailable => "semantic_worker_unavailable",
    SideRequired => "side_required",
    SourceExcluded => "source_excluded",
    SourceRevisionCollision => "source_revision_collision",
    SourceUnavailable => "source_unavailable",
    StaleHandle => "stale_handle",
    StaleResult => "stale_result",
    StaleRevision => "stale_revision",
    StaleSource => "stale_source",
    SystemUnavailable => "system_unavailable",
    TimedOut => "timed_out",
    UncertainSymbolCorrespondence => "uncertain_symbol_correspondence",
    UnknownCommand => "unknown_command",
    UnknownLanguage => "unknown_language",
    Usage => "usage",
    WorkingTreeUnavailable => "working_tree_unavailable",
    WorkspaceRequired => "workspace_required",
    WorkspaceUnavailable => "workspace_unavailable",
    WorktreeSearchIncomplete => "worktree_search_incomplete",
}
