//! Sanitized plan HTML; source HTML is text and images have no rendered output.

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
/// backslashes, `//`, any `token=` fragment, and absolute loopback links are denied.
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
        .is_some_and(|authority| !authority.is_empty() && !loopback_authority(authority))
}

/// True when an http(s) authority targets loopback: `localhost` (one trailing
/// dot tolerated), a `127.0.0.0/8` dotted quad or its single-integer form, or
/// `::1`. Userinfo and `:port` are stripped first; matching is case-insensitive.
fn loopback_authority(authority: &str) -> bool {
    let host = authority.rsplit('@').next().unwrap_or_default();
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .map(|(inside, _)| inside)
        .unwrap_or_else(|| host.split(':').next().unwrap_or_default());
    let bare = bare.strip_suffix('.').unwrap_or(bare);
    bare.eq_ignore_ascii_case("localhost")
        || bare.eq_ignore_ascii_case("::1")
        || is_loopback_quad(bare)
        || bare.parse::<u32>().is_ok_and(|n| n >> 24 == 127)
}

/// True for decimal dotted quads in `127.0.0.0/8`; short, octal, and hex
/// spellings of 127.0.0.1 are out of scope (their token-bearing forms are
/// still caught by the uniform fragment rule).
fn is_loopback_quad(host: &str) -> bool {
    let mut parts = host.split('.');
    let first_ok = parts.next() == Some("127");
    let mut rest_ok = true;
    let mut count = 0;
    for part in parts {
        count += 1;
        rest_ok &=
            !part.is_empty() && part.len() <= 3 && part.bytes().all(|byte| byte.is_ascii_digit());
    }
    first_ok && rest_ok && count == 3
}

#[cfg(test)]
mod tests;
