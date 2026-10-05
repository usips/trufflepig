//! Sanitized plan HTML; source HTML is text and images have no rendered output.

use std::borrow::Cow;

use crate::board::board_markup::{Heading, heading_model};
use pulldown_cmark::{CowStr, Event, Parser, Tag, TagEnd, html};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct RenderedPlan {
    pub html: String,
    pub headings: Vec<Heading>,
}

/// Produces the only plan markup permitted at the browser's HTML sink.
pub(crate) fn render(body: &str) -> RenderedPlan {
    let events = Parser::new(body).collect::<Vec<_>>();
    let headings = heading_model(&events);
    let mut heading_iter = headings.iter();
    let mut image_depth = 0;
    let mut links = Vec::with_capacity(1);
    let sanitized = events.into_iter().filter_map(|event| {
        match event {
            Event::Start(Tag::Image { .. }) => {
                image_depth += 1;
                return None;
            }
            Event::End(TagEnd::Image) => {
                image_depth -= 1;
                return None;
            }
            _ if image_depth > 0 => return None,
            _ => {}
        }
        match event {
            Event::Html(text) | Event::InlineHtml(text) => Some(Event::Text(text)),
            Event::Start(Tag::Heading { level, .. }) => {
                let heading = heading_iter.next().expect("shared heading traversal");
                Some(Event::Start(Tag::Heading {
                    level,
                    id: Some(CowStr::Borrowed(&heading.anchor)),
                    classes: Vec::new(),
                    attrs: Vec::new(),
                }))
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let allowed = allowed_url(&dest_url);
                links.push(allowed);
                allowed.then_some(Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                }))
            }
            Event::End(TagEnd::Link) => links
                .pop()
                .unwrap_or(false)
                .then_some(Event::End(TagEnd::Link)),
            event => Some(event),
        }
    });
    let mut rendered = String::with_capacity(body.len());
    html::push_html(&mut rendered, sanitized);
    RenderedPlan {
        html: rendered,
        headings,
    }
}

/// HTTP(S) and mailto links survive; among relative links only same-page `#…`
/// anchors do, since any other relative href navigates this origin. Controls,
/// backslashes, `//`, any `token=` fragment, and absolute links to localhost
/// or IP literals are denied.
fn allowed_url(url: &str) -> bool {
    if url
        .chars()
        .any(|character| character.is_control() || character.is_whitespace() || character == '\\')
        || url.starts_with("//")
    {
        return false;
    }
    if url
        .split_once('#')
        .is_some_and(|(_, fragment)| fragment.starts_with("token="))
    {
        return false;
    }
    let prefix = url.split(['/', '?', '#']).next().unwrap_or_default();
    let Some((scheme, _)) = prefix.split_once(':') else {
        return url.starts_with('#');
    };
    let Some(rest) = url.get(scheme.len() + 1..) else {
        return false;
    };
    if scheme.eq_ignore_ascii_case("mailto") {
        return !rest.is_empty();
    }
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    rest.strip_prefix("//")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .is_some_and(|authority| !authority.is_empty() && !blocked_authority(authority))
}

/// True when an http(s) authority targets `localhost` (one trailing dot
/// tolerated) or an IP literal: any bracketed host, or any IPv4 spelling whose
/// labels are all decimal, hex, or octal. Percent-encoding decodes first, so
/// `%31%32%37.0.0.1` and `%5b::1%5d` match; userinfo and `:port` strip after.
fn blocked_authority(authority: &str) -> bool {
    let decoded = percent_decode(authority);
    let host = decoded.rsplit('@').next().unwrap_or_default();
    if host.starts_with('[') {
        return true;
    }
    let bare = host.split(':').next().unwrap_or_default();
    let bare = bare.strip_suffix('.').unwrap_or(bare);
    bare.eq_ignore_ascii_case("localhost") || is_numeric_ipv4(bare)
}

/// True when every dot-separated label is decimal digits (`127`, which covers
/// the octal spellings) or `0x`-prefixed hex (`0x7f`); short forms (`127.1`)
/// and single integers (`2130706433`) match, having no non-numeric label.
fn is_numeric_ipv4(host: &str) -> bool {
    !host.is_empty() && host.split('.').all(is_numeric_label)
}

/// True for one IPv4 label: all ASCII digits, or `0x`/`0X` plus hex digits.
fn is_numeric_label(label: &str) -> bool {
    if let Some(hex) = label
        .strip_prefix("0x")
        .or_else(|| label.strip_prefix("0X"))
    {
        return !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_digit())
}

/// Decodes `%XX` sequences in one pass; malformed or trailing `%` stays
/// literal. Borrows back when there is nothing to decode.
fn percent_decode(value: &str) -> Cow<'_, str> {
    if !value.contains('%') {
        return Cow::Borrowed(value);
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let mut byte = bytes[index];
        let mut consumed = 1;
        if byte == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                byte = high << 4 | low;
                consumed = 3;
            }
        }
        decoded.push(byte);
        index += consumed;
    }
    Cow::Owned(String::from_utf8_lossy(&decoded).into_owned())
}

/// Value of one hex digit, or `None` for anything else.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
