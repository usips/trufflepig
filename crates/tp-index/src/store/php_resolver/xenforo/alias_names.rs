use super::alias_source::AliasSource;

#[cfg(test)]
mod tests;

/// Applies the indexed XF alias rules for lookup and grouping keys.
pub(in crate::store::php_resolver) fn canonicalize_class_name(
    raw: &str,
    source: &AliasSource,
) -> Option<String> {
    let class = raw.trim_start_matches('\\');
    if !source.issues.is_empty() && needs_alias_resolution(class, source) {
        return None;
    }
    if class.is_empty() {
        return Some(String::new());
    }
    if !source.issues.is_empty() {
        return Some(class.to_owned());
    }
    if source
        .unaliasable_namespaces
        .iter()
        .any(|namespace| begins_with_namespace(class, namespace))
    {
        return Some(class.to_owned());
    }
    if !class.contains('\\') {
        return Some(class.to_owned());
    }

    for mapping in &source.aliasable_namespaces {
        if !contains_namespace(class, &mapping.namespace) {
            continue;
        }
        if ends_with_ascii_case_insensitive(class, &mapping.suffix) {
            return Some(class.to_owned());
        }
        let mut canonical = String::with_capacity(class.len() + mapping.suffix.len());
        canonical.push_str(class);
        canonical.push_str(&mapping.suffix);
        return Some(canonical);
    }
    Some(class.to_owned())
}

/// Reports whether an incomplete source could change this lookup key.
pub(super) fn needs_alias_resolution(raw: &str, source: &AliasSource) -> bool {
    let class = raw.trim_start_matches('\\');
    if !class.contains('\\') {
        return false;
    }

    let has_global_source_issue = source.issues.iter().any(|issue| {
        matches!(
            issue,
            super::alias_source::AliasSourceIssue::SourceTooLarge
                | super::alias_source::AliasSourceIssue::InvalidEncoding
                | super::alias_source::AliasSourceIssue::ParseCancelled
                | super::alias_source::AliasSourceIssue::ParseError
                | super::alias_source::AliasSourceIssue::DepthLimit
        )
    });
    if has_global_source_issue {
        return true;
    }

    let aliasable_hook_unknown = source
        .issues
        .contains(&super::alias_source::AliasSourceIssue::AliasableHookUnsupported);
    let unaliasable_hook_unknown = source
        .issues
        .contains(&super::alias_source::AliasSourceIssue::UnaliasableHookUnsupported);

    if unaliasable_hook_unknown && needs_aliasable_suffix(class, source) {
        return true;
    }
    if aliasable_hook_unknown {
        let vendor_exclusion_is_complete = !unaliasable_hook_unknown
            && source
                .unaliasable_namespaces
                .iter()
                .any(|namespace| begins_with_namespace(class, namespace));
        return !vendor_exclusion_is_complete;
    }
    false
}

fn needs_aliasable_suffix(class: &str, source: &AliasSource) -> bool {
    for mapping in &source.aliasable_namespaces {
        if contains_namespace(class, &mapping.namespace) {
            return !ends_with_ascii_case_insensitive(class, &mapping.suffix);
        }
    }
    false
}

fn begins_with_namespace(class: &str, namespace: &str) -> bool {
    let Some(remainder) = class.get(..namespace.len()) else {
        return false;
    };
    remainder.eq_ignore_ascii_case(namespace)
        && class
            .as_bytes()
            .get(namespace.len())
            .is_some_and(|byte| *byte == b'\\')
}

fn contains_namespace(class: &str, namespace: &str) -> bool {
    if namespace.is_empty() {
        return false;
    }
    let mut needle = String::with_capacity(namespace.len() + 2);
    needle.push('\\');
    needle.push_str(namespace);
    needle.push('\\');
    class
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn ends_with_ascii_case_insensitive(value: &str, suffix: &str) -> bool {
    value
        .as_bytes()
        .get(value.len().saturating_sub(suffix.len())..)
        .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix.as_bytes()))
}
