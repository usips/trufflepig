use super::ImplementationClass;
use crate::store::php_resolver::hierarchy_index::{PhpClassKind, PhpClassRole, PhpParentStatus};
use anyhow::{Result, ensure};

pub(super) fn expected_proxy_name(raw_to_class: &str) -> Result<String> {
    let raw_to_class = raw_to_class.trim_start_matches('\\');
    let (namespace, leaf) = raw_to_class
        .rsplit_once('\\')
        .map_or((None, raw_to_class), |(namespace, leaf)| {
            (Some(namespace), leaf)
        });
    ensure!(!leaf.is_empty(), "empty XFCP implementation class leaf");
    Ok(match namespace {
        Some(namespace) if !namespace.is_empty() => format!("{namespace}\\XFCP_{leaf}"),
        _ => format!("XFCP_{leaf}"),
    })
}

pub(super) fn class_key(name: &str) -> String {
    name.trim_start_matches('\\').to_ascii_lowercase()
}

pub(super) fn proxy_readiness_issues(
    implementation: &ImplementationClass,
    expected_proxy: &str,
) -> Vec<&'static str> {
    let mut issues = Vec::with_capacity(6);
    if implementation.kind != PhpClassKind::Class {
        issues.push("implementation_not_class");
    }
    if implementation.role != PhpClassRole::Ordinary {
        issues.push("implementation_not_ordinary_class");
    }
    if !implementation.complete {
        issues.push("implementation_class_incomplete");
    }
    if implementation.conditional {
        issues.push("implementation_class_conditional");
    }
    match implementation.proxy_parent.as_ref() {
        None => issues.push("expected_proxy_parent_missing_or_ambiguous"),
        Some(parent) => {
            if !parent
                .resolved_name
                .as_deref()
                .is_some_and(|name| class_key(name) == class_key(expected_proxy))
            {
                issues.push("declared_proxy_parent_mismatch");
            }
            if parent.conditional || parent.status == PhpParentStatus::Conditional {
                issues.push("proxy_parent_conditional");
            } else if matches!(
                parent.status,
                PhpParentStatus::Incomplete | PhpParentStatus::Unsupported
            ) {
                issues.push("proxy_parent_incomplete");
            }
        }
    }
    if implementation.proxy_matches && implementation.proxy_occurrence.is_none() {
        issues.push("proxy_type_occurrence_missing");
    }
    issues
}

pub(super) fn join_reasons(reasons: &[&str]) -> String {
    let mut unique = reasons.to_vec();
    unique.sort_unstable();
    unique.dedup();
    unique.join(",")
}
