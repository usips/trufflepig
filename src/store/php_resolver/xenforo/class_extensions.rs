use super::ClassExtension;
use super::MAX_METADATA_FACTS;
use super::MAX_METADATA_SOURCE_BYTES;
use anyhow::{Context, Result, bail, ensure};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::HashMap;
use std::ops::Range;

pub(super) fn parse(source: &[u8]) -> Result<Vec<ClassExtension>> {
    parse_class_extensions(source)
}

fn parse_class_extensions(source: &[u8]) -> Result<Vec<ClassExtension>> {
    ensure!(
        source.len() <= MAX_METADATA_SOURCE_BYTES,
        "class extension XML exceeds source limit"
    );
    std::str::from_utf8(source).context("class extension XML is not UTF-8")?;
    let mut reader = Reader::from_reader(source);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::with_capacity(source.len().min(4096));
    let mut saw_root = false;
    let mut root_closed = false;
    let mut depth = 0_u8;
    let mut extensions = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buffer)?;
        let event_end = usize::try_from(reader.buffer_position())?;
        match event {
            Event::Start(element) => {
                let name = element.name();
                if !saw_root && depth == 0 {
                    ensure!(
                        name.as_ref() == b"class_extensions",
                        "unexpected class extension root"
                    );
                    ensure!(
                        element.attributes().next().is_none(),
                        "unexpected class extension root attributes"
                    );
                    saw_root = true;
                    depth = 1;
                } else {
                    bail!("class extension entries must be empty elements");
                }
            }
            Event::Empty(element) => {
                if !saw_root && depth == 0 {
                    ensure!(
                        element.name().as_ref() == b"class_extensions",
                        "unexpected class extension root"
                    );
                    ensure!(
                        element.attributes().next().is_none(),
                        "unexpected class extension root attributes"
                    );
                    saw_root = true;
                    root_closed = true;
                    buffer.clear();
                    continue;
                }
                ensure!(
                    saw_root && !root_closed && depth == 1,
                    "class extension outside root"
                );
                ensure!(
                    element.name().as_ref() == b"extension",
                    "unexpected class extension child"
                );
                if extensions.len() == MAX_METADATA_FACTS {
                    bail!("class extension row limit exceeded");
                }
                extensions.push(parse_extension_element(
                    &element, source, event_end, &reader,
                )?);
            }
            Event::End(element) => {
                ensure!(
                    saw_root && !root_closed && depth == 1,
                    "unexpected class extension closing tag"
                );
                ensure!(
                    element.name().as_ref() == b"class_extensions",
                    "unexpected class extension closing tag"
                );
                depth = 0;
                root_closed = true;
            }
            Event::Text(text) => {
                let bytes: &[u8] = text.as_ref();
                ensure!(
                    bytes.iter().all(u8::is_ascii_whitespace),
                    "unexpected class extension text"
                );
            }
            Event::CData(text) => {
                let bytes: &[u8] = text.as_ref();
                ensure!(
                    bytes.iter().all(u8::is_ascii_whitespace),
                    "unexpected class extension text"
                );
            }
            Event::DocType(_) | Event::GeneralRef(_) => {
                bail!("DTD/entity is not accepted in class extension XML")
            }
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) => {}
        }
        buffer.clear();
    }
    ensure!(saw_root && root_closed, "incomplete class extension XML");
    let mut occurrences = HashMap::<(String, String), usize>::with_capacity(extensions.len());
    for extension in &extensions {
        *occurrences
            .entry((
                class_key(&extension.from_class),
                class_key(&extension.to_class),
            ))
            .or_default() += 1;
    }
    for extension in &mut extensions {
        extension.duplicate = occurrences
            .get(&(
                class_key(&extension.from_class),
                class_key(&extension.to_class),
            ))
            .copied()
            .unwrap_or(0)
            > 1;
    }
    Ok(extensions)
}

fn class_key(name: &str) -> String {
    name.trim_start_matches('\\').to_ascii_lowercase()
}

fn parse_extension_element(
    element: &BytesStart<'_>,
    source: &[u8],
    event_end: usize,
    reader: &Reader<&[u8]>,
) -> Result<ClassExtension> {
    let tag_span = event_tag_span(source, event_end)?;
    let raw_tag = &source[tag_span.clone()];
    let spans = attribute_value_spans(raw_tag)?;
    let mut values = HashMap::<String, String>::with_capacity(5);
    for attribute in element.attributes() {
        let attribute = attribute?;
        let key = std::str::from_utf8(attribute.key.as_ref())?.to_owned();
        if matches!(
            key.as_str(),
            "from_class" | "to_class" | "active" | "execute_order"
        ) {
            let value = attribute
                .decode_and_unescape_value(reader.decoder())?
                .into_owned();
            values.insert(key, value);
        }
    }
    let from_class = values.remove("from_class").context("missing from_class")?;
    let to_class = values.remove("to_class").context("missing to_class")?;
    ensure!(
        valid_class_name(&from_class) && valid_class_name(&to_class),
        "invalid class name in extension metadata"
    );
    let from_offset = spans.get("from_class").context("missing from_class span")?;
    let to_offset = spans.get("to_class").context("missing to_class span")?;
    let active = values
        .get("active")
        .map(|value| match value.as_str() {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        })
        .unwrap_or(None);
    let execute_order = values
        .get("execute_order")
        .and_then(|value| value.parse::<i64>().ok());
    let absolute = tag_span.start;
    Ok(ClassExtension {
        from_class,
        from_span: (absolute + from_offset.start)..(absolute + from_offset.end),
        to_class,
        to_span: (absolute + to_offset.start)..(absolute + to_offset.end),
        tag_span,
        active,
        execute_order,
        duplicate: false,
    })
}

fn event_tag_span(source: &[u8], end: usize) -> Result<Range<usize>> {
    ensure!(
        end > 1 && source.get(end - 1) == Some(&b'>'),
        "invalid XML event span"
    );
    let start = source[..end]
        .iter()
        .rposition(|byte| *byte == b'<')
        .context("missing XML tag start")?;
    Ok(start..end)
}

fn attribute_value_spans(tag: &[u8]) -> Result<HashMap<String, Range<usize>>> {
    ensure!(tag.first() == Some(&b'<'), "invalid XML tag bytes");
    let mut cursor = 1;
    while cursor < tag.len()
        && !tag[cursor].is_ascii_whitespace()
        && !matches!(tag[cursor], b'/' | b'>')
    {
        cursor += 1;
    }
    let mut spans = HashMap::with_capacity(5);
    loop {
        while cursor < tag.len() && tag[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= tag.len() || matches!(tag[cursor], b'/' | b'>') {
            break;
        }
        let name_start = cursor;
        while cursor < tag.len()
            && !tag[cursor].is_ascii_whitespace()
            && !matches!(tag[cursor], b'=' | b'/' | b'>')
        {
            cursor += 1;
        }
        ensure!(cursor > name_start, "invalid XML attribute name");
        let name = std::str::from_utf8(&tag[name_start..cursor])?.to_owned();
        while cursor < tag.len() && tag[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        ensure!(
            tag.get(cursor) == Some(&b'='),
            "missing XML attribute equals"
        );
        cursor += 1;
        while cursor < tag.len() && tag[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let quote = *tag.get(cursor).context("missing XML attribute value")?;
        ensure!(matches!(quote, b'\'' | b'\"'), "unquoted XML attribute");
        cursor += 1;
        let value_start = cursor;
        while cursor < tag.len() && tag[cursor] != quote {
            cursor += 1;
        }
        ensure!(cursor < tag.len(), "unterminated XML attribute");
        spans.insert(name, value_start..cursor);
        cursor += 1;
    }
    Ok(spans)
}

fn valid_class_name(name: &str) -> bool {
    let name = name.strip_prefix('\\').unwrap_or(name);
    !name.is_empty()
        && name.split('\\').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                && segment
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        })
}
