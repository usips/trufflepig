use super::{Definition, DmDefinition, DmToken, Extraction};

#[allow(clippy::too_many_arguments)]
pub(super) fn add_definition(
    result: &mut Extraction,
    definitions: &mut Vec<DmDefinition>,
    token: &DmToken,
    end: usize,
    kind: &str,
    path: String,
    container: Option<String>,
    conditional: bool,
    local_owner: Option<usize>,
) -> usize {
    let index = result.definitions.len();
    result.definitions.push(Definition {
        name: token.text.clone(),
        kind: kind.into(),
        start: token.start,
        end,
        container,
    });
    definitions.push(DmDefinition {
        path,
        conditional,
        local_owner,
        scope_end: usize::MAX,
        name_start: token.start,
        name_end: token.end,
    });
    index
}

pub(super) fn declaration<'a>(
    tokens: &'a [DmToken],
    base: &str,
    in_proc: bool,
) -> Option<(String, usize, usize, &'a str)> {
    let absolute = tokens.first()?.text == "/";
    let mut index = usize::from(absolute);
    let mut parts = Vec::with_capacity(8);
    loop {
        let token = tokens.get(index)?;
        if !token.identifier() {
            return None;
        }
        parts.push((token.text.as_str(), index));
        index += 1;
        if tokens.get(index).is_none_or(|t| t.text != "/") {
            break;
        }
        index += 1;
    }
    let next = tokens.get(index).map(|t| t.text.as_str());
    let variable = parts.iter().any(|(s, _)| *s == "var") || base.ends_with("/var");
    let namespace = parts
        .last()
        .is_some_and(|(s, _)| matches!(*s, "proc" | "verb" | "var"));
    let callable = next == Some("(");
    if in_proc && !variable {
        return None;
    }
    if !callable && !variable && !matches!(next, None | Some("{" | "}" | ";")) {
        return None;
    }
    if matches!(
        parts[0].0,
        "return"
            | "if"
            | "else"
            | "for"
            | "while"
            | "switch"
            | "break"
            | "continue"
            | "spawn"
            | "set"
    ) {
        return None;
    }
    let kind = if namespace {
        "namespace"
    } else if variable {
        if in_proc { "binding" } else { "field" }
    } else if callable {
        if parts.iter().any(|(s, _)| *s == "verb") || base.ends_with("/verb") {
            "verb"
        } else {
            "proc"
        }
    } else {
        "type"
    };
    let mut path = if absolute {
        String::new()
    } else {
        base.trim_end_matches("/proc")
            .trim_end_matches("/verb")
            .trim_end_matches("/var")
            .to_owned()
    };
    for (part, _) in &parts {
        if !namespace && matches!(*part, "proc" | "verb" | "var") {
            continue;
        }
        path.push('/');
        path.push_str(part);
    }
    Some((path, parts.last()?.1, index, kind))
}

pub(super) fn recover_parameters(
    result: &mut Extraction,
    definitions: &mut Vec<DmDefinition>,
    tokens: &[DmToken],
    open: usize,
    owner: usize,
    conditional: bool,
) {
    let Some(close) = tokens[open..]
        .iter()
        .position(|t| t.text == ")")
        .map(|n| n + open)
    else {
        return;
    };
    for parameter in tokens[open + 1..close].split(|t| t.text == ",") {
        let before_default = parameter.split(|t| t.text == "=").next().unwrap_or(&[]);
        if let Some(name) = before_default.iter().rev().find(|t| t.identifier()) {
            let path = format!("{}/{}", definitions[owner].path, name.text);
            add_definition(
                result,
                definitions,
                name,
                name.end,
                "parameter",
                path,
                Some(definitions[owner].path.clone()),
                conditional,
                Some(owner),
            );
        }
    }
}
