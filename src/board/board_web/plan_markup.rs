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

fn allowed_url(url: &str) -> bool {
    if url
        .chars()
        .any(|character| character.is_control() || character.is_whitespace() || character == '\\')
        || url.starts_with("//")
    {
        return false;
    }
    let prefix = url.split(['/', '?', '#']).next().unwrap_or_default();
    let Some((scheme, _)) = prefix.split_once(':') else {
        return true;
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
        .is_some_and(|authority| !authority.is_empty())
}

#[cfg(test)]
mod tests;
