use std::time::{Duration, Instant};

use tree_sitter::{Node, ParseOptions, Parser};

#[cfg(test)]
mod tests;

const MAX_ALIAS_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ALIAS_ENTRIES: usize = 4_096;
const MAX_ALIAS_PARSE_DEPTH: usize = 256;
const MAX_ALIAS_PARSE_TIME: Duration = Duration::from_millis(250);

/// Literal XenForo alias rules declared by indexed PHP source.
pub(in crate::store::php_resolver) struct AliasSource {
    pub(super) aliasable_namespaces: Vec<AliasableNamespace>,
    pub(super) unaliasable_namespaces: Vec<String>,
    pub(super) issues: Vec<AliasSourceIssue>,
}

/// One ordered namespace and suffix pair from `getAliasableNamespaces()`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AliasableNamespace {
    pub(super) namespace: String,
    pub(super) suffix: String,
}

/// A bounded source condition that prevents trusting one or more alias rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AliasSourceIssue {
    SourceTooLarge,
    InvalidEncoding,
    ParseCancelled,
    ParseError,
    DepthLimit,
    AliasableHookUnsupported,
    UnaliasableHookUnsupported,
}

impl AliasSourceIssue {
    #[cfg(test)]
    pub(super) const fn code(self) -> &'static str {
        match self {
            Self::SourceTooLarge => "xenforo_alias_source_too_large",
            Self::InvalidEncoding => "xenforo_alias_source_invalid_encoding",
            Self::ParseCancelled => "xenforo_alias_source_parse_cancelled",
            Self::ParseError => "xenforo_alias_source_parse_error",
            Self::DepthLimit => "xenforo_alias_source_depth_limit",
            Self::AliasableHookUnsupported => "xenforo_get_aliasable_namespaces_unsupported",
            Self::UnaliasableHookUnsupported => "xenforo_get_unaliasable_namespaces_unsupported",
        }
    }
}

impl AliasSource {
    /// Parses only literal return arrays; this never evaluates PHP source.
    pub(in crate::store::php_resolver) fn parse(source: &[u8]) -> Self {
        let mut parsed = Self {
            aliasable_namespaces: Vec::new(),
            unaliasable_namespaces: Vec::new(),
            issues: Vec::with_capacity(2),
        };
        if source.len() > MAX_ALIAS_SOURCE_BYTES {
            parsed.issues.push(AliasSourceIssue::SourceTooLarge);
            return parsed;
        }
        if std::str::from_utf8(source).is_err() {
            parsed.issues.push(AliasSourceIssue::InvalidEncoding);
            return parsed;
        }

        let language = tree_sitter_php::LANGUAGE_PHP.into();
        let mut parser = Parser::new();
        if parser.set_language(&language).is_err() {
            parsed.issues.push(AliasSourceIssue::ParseError);
            return parsed;
        }
        let started = Instant::now();
        let mut cancelled = |_: &tree_sitter::ParseState| started.elapsed() >= MAX_ALIAS_PARSE_TIME;
        let options = ParseOptions::new().progress_callback(&mut cancelled);
        let tree =
            parser.parse_with_options(&mut |offset, _| &source[offset..], None, Some(options));
        let Some(tree) = tree else {
            parsed.issues.push(AliasSourceIssue::ParseCancelled);
            return parsed;
        };
        let root = tree.root_node();
        if root.has_error() {
            parsed.issues.push(AliasSourceIssue::ParseError);
            return parsed;
        }
        if !bounded_tree(root) {
            parsed.issues.push(AliasSourceIssue::DepthLimit);
            return parsed;
        }

        let mut aliasable = HookParse::<Vec<AliasableNamespace>>::default();
        let mut unaliasable = HookParse::<Vec<String>>::default();
        let mut cursor = root.walk();
        let mut depth = 0_usize;
        loop {
            let node = cursor.node();
            if node.kind() == "class_declaration"
                && is_global_class(node, source)
                && node
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source).ok())
                    .is_some_and(|name| name.eq_ignore_ascii_case("XF"))
            {
                collect_xf_hooks(node, source, &mut aliasable, &mut unaliasable);
            }

            if cursor.goto_first_child() {
                depth += 1;
                if depth > MAX_ALIAS_PARSE_DEPTH {
                    parsed.issues.push(AliasSourceIssue::DepthLimit);
                    return parsed;
                }
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    finish_hook(
                        aliasable,
                        AliasSourceIssue::AliasableHookUnsupported,
                        &mut parsed.aliasable_namespaces,
                        &mut parsed.issues,
                    );
                    finish_hook(
                        unaliasable,
                        AliasSourceIssue::UnaliasableHookUnsupported,
                        &mut parsed.unaliasable_namespaces,
                        &mut parsed.issues,
                    );
                    return parsed;
                }
                depth -= 1;
            }
        }
    }
}

#[derive(Default)]
struct HookParse<T> {
    count: usize,
    value: Option<T>,
}

impl<T> HookParse<T> {
    fn add(&mut self, value: Option<T>) {
        self.count += 1;
        if self.count == 1 {
            self.value = value;
        } else {
            self.value = None;
        }
    }
}

fn collect_xf_hooks(
    class: Node<'_>,
    source: &[u8],
    aliasable: &mut HookParse<Vec<AliasableNamespace>>,
    unaliasable: &mut HookParse<Vec<String>>,
) {
    let Some(body) = class.child_by_field_name("body") else {
        return;
    };
    for index in 0..body.named_child_count() {
        let Some(method) = body.named_child(index) else {
            continue;
        };
        if method.kind() != "method_declaration" {
            continue;
        }
        let Some(name) = method
            .child_by_field_name("name")
            .and_then(|name| name.utf8_text(source).ok())
        else {
            continue;
        };
        if name.eq_ignore_ascii_case("getAliasableNamespaces") {
            aliasable.add(parse_aliasable_hook(method, source));
        } else if name.eq_ignore_ascii_case("getUnaliasableNamespaces") {
            unaliasable.add(parse_unaliasable_hook(method, source));
        }
    }
}

fn parse_aliasable_hook(method: Node<'_>, source: &[u8]) -> Option<Vec<AliasableNamespace>> {
    let array = literal_return_array(method)?;
    let mut entries =
        Vec::<AliasableNamespace>::with_capacity(array.named_child_count().min(MAX_ALIAS_ENTRIES));
    let mut keyed_positions = std::collections::HashMap::<String, usize>::new();
    for index in 0..array.named_child_count() {
        if entries.len() == MAX_ALIAS_ENTRIES {
            return None;
        }
        let initializer = array.named_child(index)?;
        if initializer.kind() != "array_element_initializer" {
            return None;
        }
        let child_count = initializer.named_child_count();
        if !(child_count == 1 || child_count == 2) {
            return None;
        }
        let (key, value_node) = if child_count == 1 {
            (None, initializer.named_child(0)?)
        } else {
            (
                Some(initializer.named_child(0)?),
                initializer.named_child(1)?,
            )
        };
        let value = literal_name(value_node, source, false)?;
        let (namespace, suffix, explicit_string_key) = match key {
            Some(key) => match literal_array_key(key, source)? {
                LiteralArrayKey::String(key) => (key.clone(), value, Some(key)),
                LiteralArrayKey::NonString => (value.clone(), value, None),
            },
            None => (value.clone(), value, None),
        };
        if !valid_namespace(&namespace) {
            return None;
        }
        if let Some(key) = explicit_string_key {
            if let Some(position) = keyed_positions.get(&key).copied() {
                entries[position].suffix = suffix;
                continue;
            }
            keyed_positions.insert(key, entries.len());
        }
        entries.push(AliasableNamespace { namespace, suffix });
    }
    Some(entries)
}

fn parse_unaliasable_hook(method: Node<'_>, source: &[u8]) -> Option<Vec<String>> {
    let array = literal_return_array(method)?;
    let mut namespaces = Vec::with_capacity(array.named_child_count().min(MAX_ALIAS_ENTRIES));
    for index in 0..array.named_child_count() {
        if namespaces.len() == MAX_ALIAS_ENTRIES {
            return None;
        }
        let initializer = array.named_child(index)?;
        if initializer.kind() != "array_element_initializer" || initializer.named_child_count() != 1
        {
            return None;
        }
        namespaces.push(literal_name(initializer.named_child(0)?, source, true)?);
    }
    Some(namespaces)
}

fn literal_return_array<'tree>(method: Node<'tree>) -> Option<Node<'tree>> {
    let body = method.child_by_field_name("body")?;
    let mut return_expression = None;
    for index in 0..body.named_child_count() {
        let statement = body.named_child(index)?;
        if statement.kind() == "comment" {
            continue;
        }
        if statement.kind() != "return_statement" || return_expression.is_some() {
            return None;
        }
        if statement.named_child_count() != 1 {
            return None;
        }
        return_expression = statement.named_child(0);
    }
    let expression = return_expression?;
    if expression.kind() != "array_creation_expression" {
        return None;
    }
    Some(expression)
}

enum LiteralArrayKey {
    String(String),
    NonString,
}

fn literal_array_key(node: Node<'_>, source: &[u8]) -> Option<LiteralArrayKey> {
    if node.kind() == "string" {
        let value = decode_php_string(node.utf8_text(source).ok()?)?;
        if !value.is_empty()
            && value.as_bytes()[0].is_ascii_digit()
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && (value == "0" || !value.starts_with('0'))
            && value.parse::<i64>().is_ok()
        {
            return Some(LiteralArrayKey::NonString);
        }
        return Some(LiteralArrayKey::String(value));
    }
    matches!(node.kind(), "integer" | "float" | "boolean" | "null")
        .then_some(LiteralArrayKey::NonString)
}

fn literal_name(node: Node<'_>, source: &[u8], namespace: bool) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let value = decode_php_string(node.utf8_text(source).ok()?)?;
    let valid = if namespace {
        valid_namespace(&value)
    } else {
        valid_suffix(&value)
    };
    valid.then_some(value)
}

fn decode_php_string(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let quote = *bytes.first()?;
    if bytes.len() < 2 || bytes.last().copied()? != quote || !matches!(quote, b'\'' | b'"') {
        return None;
    }
    let content = &bytes[1..bytes.len() - 1];
    let mut decoded = Vec::with_capacity(content.len());
    let mut index = 0;
    while index < content.len() {
        let byte = content[index];
        index += 1;
        if byte != b'\\' {
            decoded.push(byte);
            continue;
        }
        let next = *content.get(index)?;
        index += 1;
        if quote == b'\'' {
            if next == b'\\' || next == b'\'' {
                decoded.push(next);
            } else {
                decoded.push(b'\\');
                decoded.push(next);
            }
            continue;
        }
        match next {
            b'\\' | b'"' | b'$' => decoded.push(next),
            b'n' => decoded.push(b'\n'),
            b'r' => decoded.push(b'\r'),
            b't' => decoded.push(b'\t'),
            b'v' => decoded.push(0x0b),
            b'e' => decoded.push(0x1b),
            b'f' => decoded.push(0x0c),
            b'x' => {
                let start = index;
                while index < content.len()
                    && index - start < 2
                    && content[index].is_ascii_hexdigit()
                {
                    index += 1;
                }
                if start == index {
                    decoded.extend_from_slice(b"\\x");
                } else {
                    decoded.push(
                        u8::from_str_radix(std::str::from_utf8(&content[start..index]).ok()?, 16)
                            .ok()?,
                    );
                }
            }
            b'0'..=b'7' => {
                let start = index - 1;
                let mut end = index;
                while end < content.len()
                    && end - start < 3
                    && (b'0'..=b'7').contains(&content[end])
                {
                    end += 1;
                }
                index = end;
                let octal = std::str::from_utf8(&content[start..end]).ok()?;
                decoded.push(u8::from_str_radix(octal, 8).ok()?);
            }
            other => {
                decoded.push(b'\\');
                decoded.push(other);
            }
        }
    }
    String::from_utf8(decoded).ok()
}

fn valid_namespace(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('\\')
        && !value.ends_with('\\')
        && value.split('\\').all(valid_identifier)
}

fn valid_suffix(value: &str) -> bool {
    valid_identifier(value)
}

fn valid_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn finish_hook<T>(
    hook: HookParse<T>,
    unsupported: AliasSourceIssue,
    destination: &mut T,
    issues: &mut Vec<AliasSourceIssue>,
) {
    if hook.count == 0 {
        return;
    }
    if let Some(value) = hook.value {
        *destination = value;
    } else {
        issues.push(unsupported);
    }
}

fn bounded_tree(root: Node<'_>) -> bool {
    let mut cursor = root.walk();
    let mut depth = 0_usize;
    loop {
        if cursor.goto_first_child() {
            depth += 1;
            if depth > MAX_ALIAS_PARSE_DEPTH {
                return false;
            }
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return true;
            }
            depth -= 1;
        }
    }
}

fn is_global_class(class: Node<'_>, source: &[u8]) -> bool {
    let mut ancestor = class.parent();
    while let Some(node) = ancestor {
        if node.kind() == "namespace_definition" {
            return node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source).ok())
                .is_none_or(str::is_empty);
        }
        ancestor = node.parent();
    }

    let mut root = class;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    let class_start = class.start_byte();
    let mut latest_unbracketed_namespace = None::<(usize, bool)>;
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.kind() == "namespace_definition"
            && node.child_by_field_name("body").is_none()
            && node.start_byte() < class_start
            && latest_unbracketed_namespace.is_none_or(|(start, _)| node.start_byte() > start)
        {
            let global = node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source).ok())
                .is_none_or(str::is_empty);
            latest_unbracketed_namespace = Some((node.start_byte(), global));
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return latest_unbracketed_namespace.is_none_or(|(_, global)| global);
            }
        }
    }
}
