use crate::{
    cli::{Arguments, parse},
    daemon::pool::{RequestClass, RequestLane},
    search::Query,
    workspace::{config::WorkspaceConfig, member_root::MemberRoot},
};
use anyhow::{Context, Result, ensure};
use std::path::PathBuf;

pub(crate) struct ClassifiedDaemonRequest {
    pub class: RequestClass,
    /// Every root a workspace search may reach, including fallback widening.
    pub roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WorkspaceSelector {
    Home,
    All,
    Member(String),
}

pub(crate) fn classify(args: &[String]) -> Result<ClassifiedDaemonRequest> {
    let Ok(options) = parse(args) else {
        return Ok(classified(RequestLane::Query, "unknown", Vec::new()));
    };
    let Some(verb) = options.words.first().map(String::as_str) else {
        return Ok(classified(RequestLane::Query, "status", Vec::new()));
    };

    if verb == "search" {
        let config = crate::workspace::resolve(&options)?;
        let (words, selector) = split_search_scope(&options.words, options.member.as_deref())?;
        let query_text = words[1..].join(" ");
        let query = Query::parse(&query_text)?;
        let semantic_shape = !query.exact && !query.regex && !query_text.starts_with("refs:");
        let semantic =
            (options.sem || config.as_ref().is_some_and(|c| c.semantic.enabled)) && !options.no_sem;
        let rerank = (options.rerank || config.as_ref().is_some_and(|c| c.semantic.rerank))
            && !options.no_rerank;
        let lane = if query.regex {
            RequestLane::Regex
        } else if semantic_shape && (semantic || rerank) {
            RequestLane::Semantic
        } else {
            RequestLane::Query
        };
        let roots = if lane == RequestLane::Query {
            Vec::new()
        } else {
            protected_search_roots(&options, config.as_ref(), selector.as_ref())?
        };
        return Ok(classified(lane, "search", roots));
    }

    let lane = match verb {
        "index"
        | "init"
        | "hist-index"
        | "semantic-check"
        | "forget-logs"
        | "semantic-worker-serve" => RequestLane::Maintenance,
        "semantic" if semantic_work(&options.words) => RequestLane::Maintenance,
        _ => RequestLane::Query,
    };
    let roots = if lane == RequestLane::Maintenance {
        let config = crate::workspace::resolve(&options)?;
        let workspace_sweep = verb == "semantic"
            && options
                .words
                .get(1)
                .is_some_and(|command| command == "prepare");
        protected_owner_roots(&options, config.as_ref(), workspace_sweep)?
    } else {
        Vec::new()
    };
    Ok(classified(lane, safe_verb(verb), roots))
}

fn classified(
    lane: RequestLane,
    verb: &'static str,
    roots: Vec<PathBuf>,
) -> ClassifiedDaemonRequest {
    ClassifiedDaemonRequest {
        class: RequestClass { lane, verb },
        roots,
    }
}

fn safe_verb(verb: &str) -> &'static str {
    match verb {
        "index" => "index",
        "init" => "init",
        "hist-index" => "hist-index",
        "semantic-check" => "semantic-check",
        "forget-logs" => "forget-logs",
        "semantic-worker-serve" => "semantic-worker-serve",
        "semantic" => "semantic",
        "search" => "search",
        "show" => "show",
        "more" => "more",
        "ctx" => "ctx",
        "refs" => "refs",
        "map" => "map",
        "status" => "status",
        "hist-status" => "hist-status",
        "hist" => "hist",
        "since" => "since",
        "diff" => "diff",
        "blame" => "blame",
        "session" => "session",
        "audit" => "audit",
        "doctor" => "doctor",
        _ => "unknown",
    }
}

fn semantic_work(words: &[String]) -> bool {
    match words.get(1).map(String::as_str) {
        Some("status") => false,
        Some("worker") => words.get(2).is_some_and(|verb| verb == "stop"),
        _ => true,
    }
}

fn split_search_scope(
    words: &[String],
    member_flag: Option<&str>,
) -> Result<(Vec<String>, Option<WorkspaceSelector>)> {
    let mut selection = member_flag.map(|name| WorkspaceSelector::Member(name.to_owned()));
    let mut query_words = Vec::with_capacity(words.len());
    for argument in words {
        let mut remaining = String::with_capacity(argument.len());
        for chunk in argument.split_inclusive(char::is_whitespace) {
            let word = chunk.trim_end();
            let selector = if word == "ws:home" {
                Some(WorkspaceSelector::Home)
            } else if word == "ws:all" {
                Some(WorkspaceSelector::All)
            } else {
                word.strip_prefix("in:")
                    .map(|name| WorkspaceSelector::Member(name.to_owned()))
            };
            if let Some(selector) = selector {
                ensure!(
                    selection.as_ref().is_none_or(|old| old == &selector),
                    "invalid_scope: conflicting workspace selectors"
                );
                selection = Some(selector);
            } else {
                remaining.push_str(chunk);
            }
        }
        if !remaining.trim().is_empty() {
            query_words.push(remaining.trim_end().to_owned());
        }
    }
    Ok((query_words, selection))
}

fn protected_search_roots(
    options: &Arguments,
    config: Option<&WorkspaceConfig>,
    selector: Option<&WorkspaceSelector>,
) -> Result<Vec<PathBuf>> {
    let start = options
        .root
        .canonicalize()
        .context("invalid_root: cannot resolve request root")?;
    let Some(config) = config else {
        ensure!(
            selector.is_none() && options.member.is_none(),
            "workspace_required: select a workspace configuration"
        );
        return Ok(vec![start]);
    };
    let roots = config.member_roots(&start)?;
    match selector {
        Some(WorkspaceSelector::Home) => one_root(
            roots
                .iter()
                .find(|root| root.is_home)
                .context("member_required: ws:home has no home member")?,
        ),
        Some(WorkspaceSelector::Member(name)) => one_root(
            roots
                .iter()
                .find(|root| root.name() == name)
                .with_context(|| format!("invalid_member: member {name} is not configured"))?,
        ),
        Some(WorkspaceSelector::All) | None => {
            Ok(roots.into_iter().map(|root| root.root).collect())
        }
    }
}

fn protected_owner_roots(
    options: &Arguments,
    config: Option<&WorkspaceConfig>,
    workspace_sweep: bool,
) -> Result<Vec<PathBuf>> {
    let start = options
        .root
        .canonicalize()
        .context("invalid_root: cannot resolve request root")?;
    let Some(config) = config else {
        ensure!(
            options.member.is_none(),
            "workspace_required: select a workspace configuration"
        );
        return Ok(vec![start]);
    };
    let roots = config.member_roots(&start)?;
    if workspace_sweep && options.member.is_none() {
        return Ok(roots.into_iter().map(|root| root.root).collect());
    }
    let selected = if let Some(name) = &options.member {
        roots
            .iter()
            .find(|root| root.name() == name)
            .with_context(|| format!("invalid_member: member {name} is not configured"))?
    } else {
        roots
            .iter()
            .find(|root| root.is_home)
            .context("member_required: select --member for an owner-scoped command")?
    };
    one_root(selected)
}

fn one_root(member: &MemberRoot) -> Result<Vec<PathBuf>> {
    Ok(vec![member.root.clone()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_selectors_are_removed_before_regex_parsing() {
        let (words, selection) = split_search_scope(
            &[
                "search".into(),
                "kind:function ws:all".into(),
                "re:needle".into(),
            ],
            None,
        )
        .unwrap();
        assert_eq!(selection, Some(WorkspaceSelector::All));
        assert_eq!(words, ["search", "kind:function", "re:needle"]);
        assert!(Query::parse(&words[1..].join(" ")).unwrap().regex);
    }

    #[test]
    fn conflicting_workspace_selectors_remain_an_error() {
        let error = split_search_scope(
            &["search".into(), "in:engine".into(), "ws:all".into()],
            None,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("invalid_scope"));
    }

    #[test]
    fn workspace_configuration_errors_do_not_fall_back_to_query_lane() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("missing");
        let args = [
            "--workspace".to_owned(),
            scratch
                .path()
                .join("missing.toml")
                .to_string_lossy()
                .into_owned(),
            "--root".to_owned(),
            root.to_string_lossy().into_owned(),
            "search".to_owned(),
            "ws:all".to_owned(),
            "re:needle".to_owned(),
        ];
        assert!(classify(&args).is_err());
    }
}
