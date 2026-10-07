use super::*;

#[test]
fn package_manifest_is_not_served() {
    let fixture = render_fixture();
    let asset = render_get(&fixture, "/board_web_main.js");
    assert!(asset.starts_with("HTTP/1.1 200 "), "{asset}");
    let manifest = render_get(&fixture, "/package.json");
    assert!(manifest.starts_with("HTTP/1.1 404 "), "{manifest}");
    assert!(manifest.contains("route not found"), "{manifest}");
}

/// Raw ES import specifiers in one served script, skipping comments and strings.
fn asset_import_specifiers(script: &str) -> Vec<&str> {
    let bytes = script.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    let quoted_end = |mut end: usize| {
        let quote = bytes[end];
        end += 1;
        while end < bytes.len() {
            if quote == b'`' && bytes[end] == b'$' && bytes.get(end + 1) == Some(&b'{') {
                let mut depth = 1;
                end += 2;
                while end < bytes.len() && depth > 0 {
                    if bytes[end] == b'{' {
                        depth += 1;
                    } else if bytes[end] == b'}' {
                        depth -= 1;
                    } else if bytes[end] == b'\\' {
                        end += 1;
                    }
                    end += 1;
                }
                continue;
            }
            if bytes[end] == b'\\' {
                end += 2;
                continue;
            }
            if bytes[end] == quote {
                return end;
            }
            end += 1;
        }
        bytes.len() - 1
    };
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$';
    let is_boundary = |at: usize| at == 0 || (!is_word(bytes[at - 1]) && bytes[at - 1] != b'.');
    while at < bytes.len() {
        let rest = &bytes[at..];
        if rest.starts_with(b"//") {
            at += rest
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(rest.len(), |line| line + 1);
        } else if rest.starts_with(b"/*") {
            let mut end = at + 2;
            while end + 1 < bytes.len() && (bytes[end] != b'*' || bytes[end + 1] != b'/') {
                end += 1;
            }
            at = (end + 2).min(bytes.len());
        } else if matches!(bytes[at], b'\'' | b'"' | b'`') {
            at = quoted_end(at) + 1;
        } else if rest.starts_with(b"from")
            && is_boundary(at)
            && !is_word(*rest.get(4).unwrap_or(&b' '))
        {
            let mut head = at + 4;
            while head < bytes.len() && bytes[head].is_ascii_whitespace() {
                head += 1;
            }
            if head < bytes.len() && matches!(bytes[head], b'\'' | b'"') {
                let end = quoted_end(head);
                found.push(&script[head + 1..end]);
                at = end + 1;
            } else {
                at += 4;
            }
        } else if rest.starts_with(b"import")
            && is_boundary(at)
            && !is_word(*rest.get(6).unwrap_or(&b' '))
        {
            let mut head = at + 6;
            while head < bytes.len() && bytes[head].is_ascii_whitespace() {
                head += 1;
            }
            if head < bytes.len() && bytes[head] == b'(' {
                head += 1;
                while head < bytes.len() && bytes[head].is_ascii_whitespace() {
                    head += 1;
                }
            }
            if head < bytes.len() && matches!(bytes[head], b'\'' | b'"' | b'`') {
                let end = quoted_end(head);
                found.push(&script[head + 1..end]);
                at = end + 1;
            } else {
                at += 6;
            }
        } else {
            at += 1;
        }
    }
    found
}

/// Resolve a relative import against the importing script's served directory.
fn resolve_asset_specifier(path: &str, specifier: &str) -> String {
    let mut segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    segments.pop();
    for part in specifier.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(part),
        }
    }
    format!("/{}", segments.join("/"))
}

/// Every ES import across the served scripts is a relative path the server serves.
#[test]
fn asset_imports_resolve_to_served_paths() {
    let scripts = [
        ("/board_web_main.js", PUBLIC_MAIN),
        ("/board_dom.js", PUBLIC_DOM),
        ("/board_views.js", PUBLIC_VIEWS),
        ("/board_cards.js", PUBLIC_CARDS),
        ("/board_routing.js", PUBLIC_ROUTING),
        ("/board_render_loop.js", PUBLIC_RENDER_LOOP),
        ("/pages/board_pages.js", PUBLIC_PAGES),
        ("/pages/plan_page.js", PUBLIC_PLAN_PAGE),
        ("/pages/proposal_page.js", PUBLIC_PROPOSAL_PAGE),
        ("/stream/board_stream.js", PUBLIC_STREAM),
        ("/stream/stream_election.js", PUBLIC_STREAM_ELECTION),
        ("/stream/stream_parse.js", PUBLIC_STREAM_PARSE),
        ("/feedback_triage.js", PUBLIC_TRIAGE),
        ("/board_reader.js", PUBLIC_READER),
        ("/board_entries.js", PUBLIC_ENTRIES),
        ("/board_web_token.js", PUBLIC_TOKEN),
        ("/board_ingest.js", PUBLIC_INGEST),
        ("/board_lru.js", PUBLIC_LRU),
        ("/board_seen.js", PUBLIC_SEEN),
    ];
    let mut checked = 0;
    for (path, script) in scripts {
        for specifier in asset_import_specifiers(script) {
            assert!(
                specifier.starts_with("./") || specifier.starts_with("../"),
                "{path} imports {specifier:?}: asset imports stay relative so node tests load them"
            );
            let resolved = resolve_asset_specifier(path, specifier);
            assert!(
                public_asset(&resolved).is_some(),
                "{path} imports {specifier:?}: {resolved} is not served"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "the asset import scanner matched nothing");
}
