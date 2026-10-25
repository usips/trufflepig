//! Typed internal facts shared by PHP extraction and store resolution.

use super::Relationship;
use std::ops::Range;

pub(crate) const MARKER_KIND: &str = "__trufflepig_php_marker_v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClassRole {
    Ordinary,
    Proxy,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParentKind {
    Ordinary,
    Proxy,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Visibility {
    Public,
    Protected,
    Private,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Marker {
    Class {
        role: ClassRole,
        conditional: bool,
    },
    Parent {
        kind: ParentKind,
        conditional: bool,
        resolved_name: Option<String>,
    },
    TraitUse {
        conditional: bool,
    },
    Method {
        visibility: Visibility,
        abstract_method: bool,
        conditional: bool,
    },
    ParentCall,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DecodedMarker {
    pub source: i64,
    pub target: Option<i64>,
    pub start: i64,
    pub end: i64,
    pub marker: Marker,
}

pub(crate) fn encode(
    source_def: usize,
    target_def: Option<usize>,
    span: Range<usize>,
    marker: Marker,
) -> Relationship {
    debug_assert!(span.start <= span.end);
    debug_assert_eq!(
        target_def.is_some(),
        matches!(&marker, Marker::Method { .. } | Marker::ParentCall)
    );
    Relationship {
        source: source_def,
        target: target_def,
        kind: MARKER_KIND.to_owned(),
        evidence_start: span.start,
        evidence_end: span.end,
        provenance: encode_payload(marker),
    }
}

#[cfg(test)]
pub(crate) fn is_marker(relationship: &Relationship) -> bool {
    relationship.kind == MARKER_KIND
}

#[cfg(test)]
pub(crate) fn decode(relationship: &Relationship) -> Option<Marker> {
    if !is_marker(relationship) {
        return None;
    }
    let marker = decode_payload(&relationship.provenance)?;
    (relationship.target.is_some() == matches!(&marker, Marker::Method { .. } | Marker::ParentCall))
        .then_some(marker)
}

pub(crate) fn decode_fields(
    kind: &str,
    provenance: &str,
    source: i64,
    target: Option<i64>,
    start: i64,
    end: i64,
) -> Option<DecodedMarker> {
    if kind != MARKER_KIND || source <= 0 || start < 0 || end < start {
        return None;
    }
    let marker = decode_payload(provenance)?;
    if target.is_some() != matches!(&marker, Marker::Method { .. } | Marker::ParentCall) {
        return None;
    }
    Some(DecodedMarker {
        source,
        target,
        start,
        end,
        marker,
    })
}

fn encode_payload(marker: Marker) -> String {
    let mut payload = String::with_capacity(40);
    payload.push_str("php_fact_v1:");
    match marker {
        Marker::Class { role, conditional } => {
            payload.push_str("class:");
            payload.push_str(class_role_name(role));
            push_flag(&mut payload, conditional);
        }
        Marker::Parent {
            kind,
            conditional,
            resolved_name,
        } => {
            payload.push_str("parent:");
            payload.push_str(parent_kind_name(kind));
            push_flag(&mut payload, conditional);
            payload.push(':');
            if let Some(name) = resolved_name {
                push_hex(&mut payload, name.as_bytes());
            } else {
                payload.push('-');
            }
        }
        Marker::TraitUse { conditional } => {
            payload.push_str("trait_use");
            push_flag(&mut payload, conditional);
        }
        Marker::Method {
            visibility,
            abstract_method,
            conditional,
        } => {
            payload.push_str("method:");
            payload.push_str(visibility_name(visibility));
            push_flag(&mut payload, abstract_method);
            push_flag(&mut payload, conditional);
        }
        Marker::ParentCall => payload.push_str("parent_call"),
    }
    payload
}

fn decode_payload(payload: &str) -> Option<Marker> {
    let mut fields = payload.strip_prefix("php_fact_v1:")?.split(':');
    let marker = match fields.next()? {
        "class" => Marker::Class {
            role: parse_class_role(fields.next()?)?,
            conditional: parse_flag(fields.next()?)?,
        },
        "parent" => Marker::Parent {
            kind: parse_parent_kind(fields.next()?)?,
            conditional: parse_flag(fields.next()?)?,
            resolved_name: parse_hex(fields.next()?)?,
        },
        "trait_use" => Marker::TraitUse {
            conditional: parse_flag(fields.next()?)?,
        },
        "method" => Marker::Method {
            visibility: parse_visibility(fields.next()?)?,
            abstract_method: parse_flag(fields.next()?)?,
            conditional: parse_flag(fields.next()?)?,
        },
        "parent_call" => Marker::ParentCall,
        _ => return None,
    };
    fields.next().is_none().then_some(marker)
}

fn class_role_name(role: ClassRole) -> &'static str {
    match role {
        ClassRole::Ordinary => "ordinary",
        ClassRole::Proxy => "proxy",
        ClassRole::Unknown => "unknown",
    }
}

fn parse_class_role(value: &str) -> Option<ClassRole> {
    match value {
        "ordinary" => Some(ClassRole::Ordinary),
        "proxy" => Some(ClassRole::Proxy),
        "unknown" => Some(ClassRole::Unknown),
        _ => None,
    }
}

fn parent_kind_name(kind: ParentKind) -> &'static str {
    match kind {
        ParentKind::Ordinary => "ordinary",
        ParentKind::Proxy => "proxy",
        ParentKind::Unknown => "unknown",
    }
}

fn parse_parent_kind(value: &str) -> Option<ParentKind> {
    match value {
        "ordinary" => Some(ParentKind::Ordinary),
        "proxy" => Some(ParentKind::Proxy),
        "unknown" => Some(ParentKind::Unknown),
        _ => None,
    }
}

fn visibility_name(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Public => "public",
        Visibility::Protected => "protected",
        Visibility::Private => "private",
        Visibility::Unknown => "unknown",
    }
}

fn parse_visibility(value: &str) -> Option<Visibility> {
    match value {
        "public" => Some(Visibility::Public),
        "protected" => Some(Visibility::Protected),
        "private" => Some(Visibility::Private),
        "unknown" => Some(Visibility::Unknown),
        _ => None,
    }
}

fn push_flag(payload: &mut String, flag: bool) {
    payload.push(':');
    payload.push(if flag { '1' } else { '0' });
}

fn parse_flag(value: &str) -> Option<bool> {
    match value {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn push_hex(payload: &mut String, bytes: &[u8]) {
    use std::fmt::Write as _;
    for byte in bytes {
        let _ = write!(payload, "{byte:02x}");
    }
}

fn parse_hex(value: &str) -> Option<Option<String>> {
    if value == "-" {
        return Some(None);
    }
    if value.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        bytes.push((high << 4) | low);
    }
    String::from_utf8(bytes).ok().map(Some)
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}
