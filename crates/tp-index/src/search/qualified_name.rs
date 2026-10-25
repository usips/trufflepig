//! `A::b` lookups: the last segment is the definition name; the qualifier must
//! name the definition's type (`impl A`, `impl Trait for A`, class `A`) or trail
//! its module path (file path components plus lexical containers).

/// A `sym:` name split into its qualifier segments and final definition name.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct QualifiedName {
    pub qualifier: Vec<String>,
    pub name: String,
}

/// How closely a definition's module path agrees with a qualifier; `None` rejects.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum QualifierMatch {
    /// The qualifier is a trailing run of the definition's path.
    Suffix,
    /// The qualifier segments occur in order within the path (re-exported items).
    Subsequence,
}

impl QualifiedName {
    /// Splits `crate::a::B::c` into `[a, B]` and `c`; `crate`, `self`, `super`
    /// and empty segments carry no location here and are dropped.
    pub(crate) fn parse(text: &str) -> Self {
        Self::parse_in(text, &[], &[])
    }

    /// Like [`Self::parse`], but a leading `crate::` resolves to `crate_root` and
    /// `self::`/`super::` to `module` (the importing module's path segments).
    pub(crate) fn parse_in(text: &str, module: &[String], crate_root: &[String]) -> Self {
        let mut segments: Vec<&str> = text
            .split("::")
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .collect();
        let name = segments.pop().unwrap_or_default().to_owned();
        let mut qualifier = Vec::with_capacity(module.len() + segments.len());
        let mut rest = segments.as_slice();
        match rest.first() {
            Some(&"crate") => {
                qualifier.extend_from_slice(crate_root);
                rest = &rest[1..];
            }
            Some(&("self" | "super")) => {
                qualifier.extend_from_slice(module);
                rest = rest.strip_prefix(&["self"]).unwrap_or(rest);
                while let Some(tail) = rest.strip_prefix(&["super"]) {
                    qualifier.pop();
                    rest = tail;
                }
            }
            _ => {}
        }
        qualifier.extend(
            rest.iter()
                .filter(|segment| !matches!(**segment, "crate" | "self" | "super"))
                .map(|segment| (*segment).to_owned()),
        );
        Self { qualifier, name }
    }

    /// The last qualifier segment, which SQL uses to pre-filter owners (`""` if none).
    pub(crate) fn owner_hint(&self) -> &str {
        self.qualifier.last().map_or("", String::as_str)
    }

    pub(crate) fn is_qualified(&self) -> bool {
        !self.qualifier.is_empty()
    }

    /// Matches the qualifier against `module path ++ container path` of one row.
    pub(crate) fn matches(&self, path: &str, container: Option<&str>) -> Option<QualifierMatch> {
        if self.qualifier.is_empty() {
            return Some(QualifierMatch::Suffix);
        }
        let mut owner = module_segments(path);
        let module_len = owner.len();
        let mut best: Option<QualifierMatch> = None;
        for scope in container_scopes(container) {
            owner.truncate(module_len);
            owner.extend(scope);
            let found = if owner.ends_with(&self.qualifier) {
                Some(QualifierMatch::Suffix)
            } else if is_subsequence(&self.qualifier, &owner) {
                Some(QualifierMatch::Subsequence)
            } else {
                None
            };
            best = match (best, found) {
                (Some(old), Some(new)) => Some(old.min(new)),
                (old, new) => old.or(new),
            };
        }
        best
    }
}

fn is_subsequence(needle: &[String], haystack: &[String]) -> bool {
    let mut remaining = haystack.iter();
    needle
        .iter()
        .all(|segment| remaining.any(|candidate| candidate == segment))
}

/// Module segments of the crate containing `path`: the components before its
/// last `src` directory (empty when there is none).
pub(crate) fn crate_root_segments(path: &str) -> Vec<String> {
    match path.rfind("src/") {
        Some(index) if index == 0 || path.as_bytes()[index - 1] == b'/' => {
            module_segments(path[..index].trim_end_matches('/'))
        }
        _ => Vec::new(),
    }
}

/// Module-like path components: directories and file stem, without `src`,
/// `mod`, `lib`, `main`, `index`, and with `-` read as `_` (crate names).
pub(crate) fn module_segments(path: &str) -> Vec<String> {
    let stem =
        path.rsplit_once('.').map_or(
            path,
            |(stem, extension)| {
                if extension.contains('/') { path } else { stem }
            },
        );
    stem.split('/')
        .filter(|part| !matches!(*part, "" | "src" | "mod" | "lib" | "main" | "index"))
        .map(|part| part.replace('-', "_"))
        .collect()
}

/// Alternative owner paths a container names: `impl Trait for Type` yields the
/// type and the trait; `a::b` nested scopes yield their segments.
fn container_scopes(container: Option<&str>) -> Vec<Vec<String>> {
    let Some(container) = container.map(str::trim).filter(|c| !c.is_empty()) else {
        return vec![Vec::new()];
    };
    if let Some(header) = impl_header(container) {
        let (trait_part, type_part) = match split_top_level_for(header) {
            Some((trait_part, type_part)) => (Some(trait_part), type_part),
            None => (None, header),
        };
        let mut scopes = Vec::with_capacity(2);
        scopes.extend(type_name(type_part).map(|name| vec![name]));
        scopes.extend(trait_part.and_then(type_name).map(|name| vec![name]));
        if scopes.is_empty() {
            scopes.push(Vec::new());
        }
        return scopes;
    }
    vec![
        container
            .split("::")
            .flat_map(|part| part.split(['\\', '.', '/']))
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect(),
    ]
}

/// The text after `impl` and its generic parameters, or `None` for other containers.
fn impl_header(container: &str) -> Option<&str> {
    let rest = container
        .strip_prefix("unsafe ")
        .unwrap_or(container)
        .strip_prefix("impl")?;
    if !rest.starts_with(['<', ' ', '\n', '\t']) {
        return None;
    }
    let rest = rest.trim_start();
    Some(match rest.strip_prefix('<') {
        Some(generics) => generics[closing_angle(generics)..].trim_start(),
        None => rest,
    })
}

/// Byte offset just past the `>` closing an already-opened `<`.
fn closing_angle(text: &str) -> usize {
    let mut depth = 1usize;
    for (offset, ch) in text.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return offset + 1;
                }
            }
            _ => {}
        }
    }
    text.len()
}

fn split_top_level_for(header: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    for (offset, ch) in header.char_indices() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            _ if depth == 0 && header[offset..].starts_with(" for ") => {
                return Some((&header[..offset], &header[offset + 5..]));
            }
            _ => {}
        }
    }
    None
}

/// Last path segment of a type expression: `&mut crate::a::Foo<T> where ..` -> `Foo`.
fn type_name(text: &str) -> Option<String> {
    let text = text.split(" where").next().unwrap_or(text);
    let text = text.split('<').next().unwrap_or(text).trim();
    let text = text
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim_start_matches("dyn ")
        .trim_start_matches('!');
    let name = text.rsplit("::").next().unwrap_or(text).trim();
    (!name.is_empty() && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_'))
        .then(|| name.to_owned())
}

/// The imported path and local alias of one import row's source text:
/// `a::b as c` -> (`a::b`, `c`), `a::b` -> (`a::b`, `b`), `x as y` in TS braces too.
pub(crate) fn import_spelling(text: &str) -> Option<(String, String)> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let (path, alias) = match text.split_once(" as ") {
        Some((path, alias)) => (path.trim(), alias.trim()),
        None => (text.as_str(), text.rsplit("::").next().unwrap_or(&text)),
    };
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | ':' | '$'))
    };
    (valid(path) && valid(alias)).then(|| (path.to_owned(), alias.to_owned()))
}

#[cfg(test)]
mod tests;
