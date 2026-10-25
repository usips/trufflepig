//! Resolves caller identity in the client process before request transport.
use super::Arguments;
use anyhow::Result;
use std::{collections::HashMap, path::PathBuf};

pub fn request_context(options: &Arguments) -> Result<tp_core::request_context::RequestContext> {
    let environment = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect::<HashMap<_, _>>();
    resolved_request_context(options, &environment, &process_ancestor_names())
}

fn resolved_request_context(
    options: &Arguments,
    environment: &HashMap<String, String>,
    ancestors: &[String],
) -> Result<tp_core::request_context::RequestContext> {
    use tp_core::request_context::{HarnessLabel, validate_actor_component};

    let detected = native_harness(environment, ancestors).or_else(|| process_harness(ancestors));
    let fallback = nonempty(environment.get("TRUFFLEPIG_AGENT_HARNESS"));
    if detected.is_none()
        && fallback
            .as_deref()
            .is_some_and(|harness| matches!(harness, "human" | "cli" | "system"))
    {
        anyhow::bail!(coded!(
            InvalidClient,
            "TRUFFLEPIG_AGENT_HARNESS cannot name reserved client identities"
        ));
    }
    let observed = detected.or(fallback);
    if observed.as_deref() == Some("system") || options.client.as_deref() == Some("system") {
        anyhow::bail!(coded!(
            InvalidClient,
            "system is reserved for trusted internal board requests"
        ));
    }
    if observed.is_some()
        && options
            .client
            .as_deref()
            .is_some_and(|client| matches!(client, "human" | "cli"))
    {
        anyhow::bail!(coded!(
            InvalidClient,
            "a detected harness cannot be overridden with --client human or --client cli"
        ));
    }

    let client = options.client.clone().or_else(|| observed.clone());
    if let Some(client) = client.as_deref() {
        HarnessLabel::parse(client)?;
    }
    let wrapper_session = nonempty(environment.get("TRUFFLEPIG_SESSION"));
    let provider_session = observed
        .as_deref()
        .and_then(|harness| native_session(environment, harness));
    let agent_session = nonempty(environment.get("AGENT_SESSION_ID"));
    let session = options
        .session
        .clone()
        .or_else(|| wrapper_session.clone())
        .or_else(|| provider_session.clone())
        .or_else(|| agent_session.clone());
    if let Some(session) = session.as_deref() {
        validate_actor_component(session, "session")?;
    }
    let origin = observed
        .as_deref()
        .map(|harness| -> Result<tp_core::request_context::ActorOrigin> {
            let origin_session = provider_session
                .clone()
                .or_else(|| wrapper_session.clone())
                .or_else(|| agent_session.clone())
                .unwrap_or_else(|| "unattributed".to_owned());
            validate_actor_component(&origin_session, "session")?;
            Ok(tp_core::request_context::ActorOrigin {
                harness: HarnessLabel::parse(harness)?,
                session: origin_session,
                peer_ip: None,
            })
        })
        .transpose()?;
    let diagnostic_session = options
        .session
        .clone()
        .or_else(|| nonempty(environment.get("TRUFFLEPIG_SESSION")));
    Ok(
        tp_core::request_context::RequestContext::with_diagnostic_session(
            session,
            client,
            origin,
            diagnostic_session,
        ),
    )
}

fn nonempty(value: Option<&String>) -> Option<String> {
    value.filter(|value| !value.is_empty()).cloned()
}

/// Chooses among the native markers present. A nested harness inherits its
/// parent's markers, so the nearest ancestor naming a marked harness wins;
/// without one, the first marker in this order decides.
fn native_harness(environment: &HashMap<String, String>, ancestors: &[String]) -> Option<String> {
    let has = |key: &str, value: &str| environment.get(key).is_some_and(|found| found == value);
    let present = |key: &str| nonempty(environment.get(key)).is_some();
    let mut marked: Vec<&'static str> = Vec::with_capacity(8);
    if has("GEMINI_CLI", "1") {
        marked.push("gemini");
    }
    if present("GROK_SESSION_ID") {
        marked.push("grok");
    }
    if has("OMPCODE", "1") {
        marked.push("omp");
    }
    if has("OPENCODE", "1") {
        marked.push("opencode");
    }
    if has("CLAUDECODE", "1") || has("CLAUDE_CODE_CHILD_SESSION", "1") {
        marked.push("claude");
    }
    if ["CODEX_THREAD_ID", "CODEX_SESSION_ID", "CODEX_VERSION"]
        .iter()
        .any(|key| present(key))
    {
        marked.push("codex");
    }
    // Only the documented session variables mark Kimi and Muse; other names
    // under those prefixes, such as API keys or model settings, do not.
    if present("KIMI_SESSION_ID") {
        marked.push("kimi");
    }
    if present("MUSE_SESSION_ID") {
        marked.push("muse");
    }
    ancestors
        .iter()
        .find_map(|comm| harness_for_process(comm).filter(|harness| marked.contains(harness)))
        .or_else(|| marked.first().copied())
        .map(str::to_owned)
}

fn process_harness(ancestors: &[String]) -> Option<String> {
    ancestors
        .iter()
        .find_map(|comm| harness_for_process(comm))
        .map(str::to_owned)
}

fn harness_for_process(comm: &str) -> Option<&'static str> {
    let lowered = comm.to_ascii_lowercase();
    if lowered == "grok" || lowered.starts_with("grok-") {
        Some("grok")
    } else if lowered == "claude" {
        Some("claude")
    } else if lowered == "codex" || lowered.starts_with("codex-") {
        Some("codex")
    } else if lowered == "omp" {
        Some("omp")
    } else if lowered == "opencode" {
        Some("opencode")
    } else if lowered.starts_with("kimi") {
        Some("kimi")
    } else if lowered.starts_with("muse") {
        Some("muse")
    } else {
        None
    }
}

fn native_session(environment: &HashMap<String, String>, harness: &str) -> Option<String> {
    let keys: &[&str] = match harness {
        "gemini" => &["GEMINI_SESSION_ID"],
        "grok" => &["GROK_SESSION_ID"],
        "omp" => &["TRUFFLEPIG_OMP_SESSION"],
        "opencode" => &["TRUFFLEPIG_OPENCODE_SESSION"],
        "claude" => &["TRUFFLEPIG_CLAUDE_SESSION"],
        "codex" => &["CODEX_THREAD_ID", "CODEX_SESSION_ID"],
        "kimi" => &["KIMI_SESSION_ID"],
        "muse" => &["MUSE_SESSION_ID"],
        _ => &[],
    };
    keys.iter().find_map(|key| nonempty(environment.get(*key)))
}

fn process_ancestor_names() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        let mut names = Vec::with_capacity(12);
        let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
            return names;
        };
        let Some(mut pid) = parent_pid(&stat) else {
            return names;
        };
        for _ in 0..12 {
            let base = PathBuf::from(format!("/proc/{pid}"));
            let Ok(comm) = std::fs::read_to_string(base.join("comm")) else {
                break;
            };
            names.push(comm.trim().to_owned());
            let Ok(stat) = std::fs::read_to_string(base.join("stat")) else {
                break;
            };
            let Some(parent) = parent_pid(&stat) else {
                break;
            };
            if parent <= 1 || parent == pid {
                break;
            }
            pid = parent;
        }
        names
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn parent_pid(stat: &str) -> Option<u32> {
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(args: &[&str]) -> Arguments {
        crate::cli::parse(
            &args
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    fn environment(values: &[(&str, &str)]) -> HashMap<String, String> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn native_markers_precede_wrapper_hints_and_keep_omp_before_claude() {
        let environment = environment(&[
            ("TRUFFLEPIG_AGENT_HARNESS", "claude"),
            ("OMPCODE", "1"),
            ("CLAUDECODE", "1"),
            ("TRUFFLEPIG_OMP_SESSION", "omp-session"),
            ("TRUFFLEPIG_SESSION", "explicit-session"),
        ]);
        let context = resolved_request_context(&arguments(&[]), &environment, &[]).unwrap();
        assert_eq!(context.client.as_deref(), Some("omp"));
        assert_eq!(context.session.as_deref(), Some("explicit-session"));
        assert_eq!(
            context.diagnostic_session.as_deref(),
            Some("explicit-session")
        );
        let origin = context.origin.unwrap();
        assert_eq!(origin.harness.as_str(), "omp");
        assert_eq!(origin.session, "omp-session");
    }

    #[test]
    fn opencode_marker_precedes_claude_and_supplies_its_plugin_session() {
        let marked = environment(&[
            ("OPENCODE", "1"),
            ("CLAUDECODE", "1"),
            ("TRUFFLEPIG_OPENCODE_SESSION", "ses_opencode"),
        ]);
        let context = resolved_request_context(&arguments(&[]), &marked, &[]).unwrap();
        assert_eq!(context.client.as_deref(), Some("opencode"));
        assert_eq!(context.session.as_deref(), Some("ses_opencode"));
        assert_eq!(context.origin.unwrap().session, "ses_opencode");

        let ancestors = ["bash".to_owned(), "opencode".to_owned()];
        let context =
            resolved_request_context(&arguments(&[]), &environment(&[]), &ancestors).unwrap();
        assert_eq!(context.client.as_deref(), Some("opencode"));
        assert_eq!(context.origin.unwrap().session, "unattributed");
    }

    #[test]
    fn detected_harness_rejects_human_cli_and_system_claims() {
        let environment = environment(&[("CODEX_THREAD_ID", "thread-7")]);
        for client in ["human", "cli", "system"] {
            let error =
                resolved_request_context(&arguments(&["--client", client]), &environment, &[])
                    .unwrap_err();
            assert!(error.to_string().starts_with("invalid_client:"));
        }
    }

    #[test]
    fn reserved_fallback_harnesses_are_rejected_without_native_detection() {
        for harness in ["human", "cli", "system"] {
            let environment = environment(&[("TRUFFLEPIG_AGENT_HARNESS", harness)]);
            let error = resolved_request_context(&arguments(&[]), &environment, &[]).unwrap_err();
            assert!(error.to_string().starts_with("invalid_client:"));
        }
    }

    #[test]
    fn native_detection_precedes_a_reserved_fallback_harness() {
        let environment = environment(&[
            ("CODEX_THREAD_ID", "thread-11"),
            ("TRUFFLEPIG_AGENT_HARNESS", "system"),
        ]);
        let context = resolved_request_context(&arguments(&[]), &environment, &[]).unwrap();
        assert_eq!(context.client.as_deref(), Some("codex"));
        assert_eq!(context.origin.unwrap().harness.as_str(), "codex");
    }

    #[test]
    fn explicit_human_and_cli_clients_are_allowed_without_observed_harness() {
        let environment = environment(&[]);
        for client in ["human", "cli"] {
            let context =
                resolved_request_context(&arguments(&["--client", client]), &environment, &[])
                    .unwrap();
            assert_eq!(context.client.as_deref(), Some(client));
            assert!(context.origin.is_none());
        }
    }

    #[test]
    fn observed_origin_is_preserved_separately_from_an_explicit_client_claim() {
        let environment = environment(&[
            ("CODEX_VERSION", "1.0"),
            ("TRUFFLEPIG_AGENT_HARNESS", "muse"),
            ("CODEX_THREAD_ID", "thread-9"),
        ]);
        let context =
            resolved_request_context(&arguments(&["--client", "muse"]), &environment, &[]).unwrap();
        assert_eq!(context.client.as_deref(), Some("muse"));
        assert_eq!(context.session.as_deref(), Some("thread-9"));
        let origin = context.origin.unwrap();
        assert_eq!(origin.harness.as_str(), "codex");
        assert_eq!(origin.session, "thread-9");
    }

    #[test]
    fn native_origin_session_survives_a_claimed_actor_session_override() {
        let environment = environment(&[("CODEX_THREAD_ID", "native-thread-9")]);
        let context = resolved_request_context(
            &arguments(&["--client", "muse", "--session", "claimed-session"]),
            &environment,
            &[],
        )
        .unwrap();
        assert_eq!(context.client.as_deref(), Some("muse"));
        assert_eq!(context.session.as_deref(), Some("claimed-session"));
        assert_eq!(
            context.diagnostic_session.as_deref(),
            Some("claimed-session")
        );
        let origin = context.origin.unwrap();
        assert_eq!(origin.harness.as_str(), "codex");
        assert_eq!(origin.session, "native-thread-9");
    }

    #[test]
    fn process_ancestors_are_checked_before_wrapper_hints() {
        let environment = environment(&[("TRUFFLEPIG_AGENT_HARNESS", "muse")]);
        let ancestors = ["bash".to_owned(), "Claude".to_owned()];
        let context = resolved_request_context(&arguments(&[]), &environment, &ancestors).unwrap();
        assert_eq!(context.client.as_deref(), Some("claude"));
        assert_eq!(context.origin.unwrap().harness.as_str(), "claude");
    }

    #[test]
    fn detected_harness_without_a_session_keeps_its_origin_unattributed() {
        let environment = environment(&[("CLAUDECODE", "1")]);
        let context = resolved_request_context(&arguments(&[]), &environment, &[]).unwrap();
        assert_eq!(context.client.as_deref(), Some("claude"));
        assert_eq!(context.session, None);
        assert_eq!(context.diagnostic_session, None);
        assert_eq!(context.origin.unwrap().session, "unattributed");
    }

    #[test]
    fn nearest_ancestor_decides_between_nested_harness_markers() {
        let environment = environment(&[
            ("CLAUDECODE", "1"),
            ("TRUFFLEPIG_CLAUDE_SESSION", "claude-parent"),
            ("CODEX_THREAD_ID", "codex-child"),
        ]);
        let codex_inside_claude = ["bash".to_owned(), "codex".to_owned(), "claude".to_owned()];
        let context =
            resolved_request_context(&arguments(&[]), &environment, &codex_inside_claude).unwrap();
        assert_eq!(context.client.as_deref(), Some("codex"));
        assert_eq!(context.session.as_deref(), Some("codex-child"));
        assert_eq!(context.origin.unwrap().session, "codex-child");

        let claude_inside_codex = ["bash".to_owned(), "claude".to_owned(), "codex".to_owned()];
        let context =
            resolved_request_context(&arguments(&[]), &environment, &claude_inside_codex).unwrap();
        assert_eq!(context.client.as_deref(), Some("claude"));
        assert_eq!(context.session.as_deref(), Some("claude-parent"));
        let origin = context.origin.unwrap();
        assert_eq!(origin.harness.as_str(), "claude");
        assert_eq!(origin.session, "claude-parent");
    }

    #[test]
    fn nested_markers_without_a_marked_ancestor_keep_the_fixed_order() {
        let environment = environment(&[
            ("CLAUDECODE", "1"),
            ("TRUFFLEPIG_CLAUDE_SESSION", "claude-parent"),
            ("CODEX_THREAD_ID", "codex-child"),
        ]);
        for ancestors in [&[][..], &["bash".to_owned(), "zsh".to_owned()][..]] {
            let context =
                resolved_request_context(&arguments(&[]), &environment, ancestors).unwrap();
            assert_eq!(context.client.as_deref(), Some("claude"));
            assert_eq!(context.session.as_deref(), Some("claude-parent"));
        }
    }

    #[test]
    fn unrelated_kimi_and_muse_variables_do_not_mark_a_shell() {
        let environment = environment(&[("KIMI_API_KEY", "key"), ("MUSE_MODEL", "model")]);
        let context = resolved_request_context(&arguments(&[]), &environment, &[]).unwrap();
        assert_eq!(context.client, None);
        assert!(context.origin.is_none());
    }

    #[test]
    fn documented_kimi_and_muse_session_markers_detect_their_harness() {
        let kimi = environment(&[("KIMI_SESSION_ID", "kimi-7")]);
        let context = resolved_request_context(&arguments(&[]), &kimi, &[]).unwrap();
        assert_eq!(context.client.as_deref(), Some("kimi"));
        assert_eq!(context.session.as_deref(), Some("kimi-7"));
        assert_eq!(context.origin.unwrap().harness.as_str(), "kimi");

        let muse = environment(&[("MUSE_SESSION_ID", "muse-7")]);
        let context = resolved_request_context(&arguments(&[]), &muse, &[]).unwrap();
        assert_eq!(context.client.as_deref(), Some("muse"));
        assert_eq!(context.session.as_deref(), Some("muse-7"));
        assert_eq!(context.origin.unwrap().harness.as_str(), "muse");
    }
}
