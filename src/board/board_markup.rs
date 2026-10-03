//! Shared visible heading titles and stable anchors for plan reads and rendering.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::Serialize;
use std::collections::{HashMap, hash_map::Entry};

/// A rendered heading; task sections match its normalized visible title exactly.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Heading {
    pub level: u8,
    pub title: String,
    pub anchor: String,
}

pub(crate) fn headings(body: &str) -> Vec<Heading> {
    heading_model(&Parser::new(body).collect::<Vec<_>>())
}

pub(super) fn heading_model(events: &[Event<'_>]) -> Vec<Heading> {
    let count = events
        .iter()
        .filter(|event| matches!(event, Event::Start(Tag::Heading { .. })))
        .count();
    let mut headings = Vec::with_capacity(count);
    let mut anchors = HeadingAnchors(HashMap::with_capacity(count));
    let mut current = None;
    let mut image_depth = 0;
    for event in events {
        match event {
            Event::Start(Tag::Image { .. }) => image_depth += 1,
            Event::End(TagEnd::Image) => image_depth -= 1,
            _ if image_depth > 0 => {}
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((*level as u8, String::new()));
            }
            Event::End(TagEnd::Heading(_)) => {
                let Some((level, title)) = current.take() else {
                    continue;
                };
                let title = normalized_title(&title);
                let anchor = anchors.next(&title);
                headings.push(Heading {
                    level,
                    title,
                    anchor,
                });
            }
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text) => {
                if let Some((_, title)) = &mut current {
                    title.push_str(text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, title)) = &mut current {
                    title.push(' ');
                }
            }
            _ => {}
        }
    }
    headings
}

fn normalized_title(source: &str) -> String {
    let mut title = String::with_capacity(source.len());
    for word in source.split_whitespace() {
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(word);
    }
    title
}

struct HeadingAnchors(HashMap<String, usize>);

impl HeadingAnchors {
    fn next(&mut self, title: &str) -> String {
        let base = heading_slug(title);
        if let Entry::Vacant(entry) = self.0.entry(base.clone()) {
            entry.insert(2);
            return base;
        }
        loop {
            let duplicate = self.0.get_mut(&base).expect("existing heading anchor");
            let anchor = format!("{base}-{duplicate}");
            *duplicate += 1;
            if let Entry::Vacant(entry) = self.0.entry(anchor.clone()) {
                entry.insert(2);
                return anchor;
            }
        }
    }
}

fn heading_slug(title: &str) -> String {
    let mut base = String::with_capacity(title.len() + 5);
    base.push_str("plan-");
    let mut separator = false;
    for character in title.chars() {
        if character.is_alphanumeric() {
            if separator && base.len() > 5 {
                base.push('-');
            }
            base.extend(character.to_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if base.len() == 5 {
        base.push_str("section");
    }
    base
}

#[cfg(test)]
mod tests;
