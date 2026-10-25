use super::{allowed_url, render};
use crate::board_markup::headings;

mod empty_hex_tests;

#[test]
fn source_html_is_escaped_and_markdown_formatting_survives() {
    let rendered = render(
        "<script>alert('block')</script>\n\n**safe** <img src=x onerror=alert(1)> `</code><script>`\n",
    );
    assert!(!rendered.html.contains("<script>"));
    assert!(!rendered.html.contains("<img"));
    assert!(
        rendered
            .html
            .contains("&lt;script&gt;alert('block')&lt;/script&gt;")
    );
    assert!(rendered.html.contains("<strong>safe</strong>"));
    assert!(rendered.html.contains("&lt;img src=x onerror=alert(1)&gt;"));
    assert!(
        rendered
            .html
            .contains("<code>&lt;/code&gt;&lt;script&gt;</code>")
    );
}

#[test]
fn images_and_their_formatted_alt_content_are_suppressed() {
    let rendered = render(
        "Before ![secret **alt** <img src=x>](data:image/svg+xml,boom) after\n\n\
         ![external](https://example.test/image.png)\n",
    );
    assert!(!rendered.html.contains("<img"));
    assert!(!rendered.html.contains("secret"));
    assert!(!rendered.html.contains("external"));
    assert!(rendered.html.contains("Before  after"));
}

#[test]
fn links_are_validated_after_entity_and_markdown_decoding() {
    let rendered = render(
        "[script](javascript:alert%281%29) [entity](jav&#x61;script:evil) \
         [control](java&#x09;script:evil) [data](data:text/html,boom) \
         [slash](https:\\\\example.test/x) [relative](../plan?x=1&amp;y=2) \
         [secure](HTTPS://example.test/) [email](mailto:owner@example.test)\n",
    );
    assert!(!rendered.html.contains("href=\"javascript"));
    assert!(!rendered.html.contains("href=\"data:"));
    assert!(!rendered.html.contains("href=\"java"));
    assert!(!rendered.html.contains("href=\"https:"));
    assert!(!rendered.html.contains("href=\"../plan"));
    assert!(rendered.html.contains("href=\"HTTPS://example.test/\""));
    assert!(rendered.html.contains("href=\"mailto:owner@example.test\""));
    assert!(rendered.html.contains("script"));
}

#[test]
fn relative_links_are_reduced_to_same_page_anchors() {
    // A same-origin path reuses the tab's origin, so href="/…#token=…" would
    // overwrite the stored session token; only in-page anchors stay linked.
    let rendered = render(
        "[x](/?ref=P1#token=junk) [y](#fine) [z](//host/x) [w](relative/path) \
         [q](?query=1) [e]() [v](P7@3)\n",
    );
    assert!(!rendered.html.contains("href=\"/"), "{}", rendered.html);
    assert!(!rendered.html.contains("href=\"relative"));
    assert!(!rendered.html.contains("href=\"?"));
    assert!(!rendered.html.contains("href=\"\""));
    assert!(!rendered.html.contains("href=\"P7"));
    assert!(
        rendered.html.contains("href=\"#fine\""),
        "{}",
        rendered.html
    );
    assert!(
        rendered
            .html
            .contains("<p>x <a href=\"#fine\">y</a> z w q e v</p>"),
        "{}",
        rendered.html
    );
}

#[test]
fn attributes_are_escaped_and_heading_mapping_matches_rendering() {
    let body = "# **Build** `API` ###\n# **Build** `API`\n\n\
                [quoted](https://example.test/?q=&quot;x&quot; \"&quot; onclick=&quot;bad\")\n";
    let rendered = render(body);
    assert_eq!(rendered.headings, headings(body));
    assert_eq!(rendered.headings[0].title, "Build API");
    assert!(
        rendered
            .html
            .contains("<h1 id=\"plan-build-api\"><strong>Build</strong> <code>API</code></h1>")
    );
    assert!(rendered.html.contains("<h1 id=\"plan-build-api-2\">"));
    assert!(rendered.html.contains("&quot;"));
    assert!(!rendered.html.contains("\" onclick=\""));
}

const TOKEN_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn token_bearing_links_render_as_text() {
    for href in [
        format!("/#token={TOKEN_HEX}"),
        format!("#token={TOKEN_HEX}"),
        format!("http://127.0.0.1:7341/#token={TOKEN_HEX}"),
    ] {
        let rendered = render(&format!("[x]({href})"));
        assert!(
            !rendered.html.contains("href="),
            "{href}: {}",
            rendered.html
        );
        assert!(rendered.html.contains(">x<"), "{href}: {}", rendered.html);
        assert!(
            !rendered.html.contains("token="),
            "{href}: {}",
            rendered.html
        );
    }
}

#[test]
fn numeric_host_links_render_as_text() {
    for href in [
        "http://127.1/",
        "http://0x7f.0.0.1/",
        "http://0177.0.0.1/",
        "http://[0:0:0:0:0:0:0:1]/",
        "http://%31%32%37.0.0.1/",
        "http://127.0.0.%31/",
        "http://0x7f.0.0.%31/",
        "http://%6c%6fcalhost/",
        "http://localhost%2E/",
        "http://%5b::1%5d/",
        "http://%5B0:0:0:0:0:0:0:1%5D/",
    ] {
        let rendered = render(&format!("[x]({href})"));
        assert!(
            !rendered.html.contains("href="),
            "{href}: {}",
            rendered.html
        );
        assert!(rendered.html.contains(">x<"), "{href}: {}", rendered.html);
    }
}

#[test]
fn loopback_links_without_tokens_render_as_text() {
    for href in ["http://localhost:7341/page", "http://[::1]:7341/"] {
        let rendered = render(&format!("[x]({href})"));
        assert!(
            !rendered.html.contains("href="),
            "{href}: {}",
            rendered.html
        );
        assert!(rendered.html.contains(">x<"), "{href}: {}", rendered.html);
    }
}

#[test]
fn unicode_loopback_hosts_and_subdomains_render_as_text() {
    for href in [
        "http://１２７.0.0.1/",
        "http://127。0。0。1/",
        "http://127．0．0．1/",
        "http://127\u{ad}.0.0.1/",
        "http://ｌｏｃａｌｈｏｓｔ/",
        "http://evil.localhost/",
        "http://evil.localhost./",
    ] {
        assert!(!allowed_url(href), "accepted {href:?}");
        let rendered = render(&format!("[x]({href})"));
        assert!(
            !rendered.html.contains("href="),
            "{href}: {}",
            rendered.html
        );
        assert!(rendered.html.contains(">x<"), "{href}: {}", rendered.html);
    }
}

#[test]
fn safe_links_still_link() {
    let rendered = render("[x](#section) [y](https://example.com/) [z](mailto:a@b.example)");
    assert!(
        rendered.html.contains("href=\"#section\""),
        "{}",
        rendered.html
    );
    assert!(
        rendered.html.contains("href=\"https://example.com/\""),
        "{}",
        rendered.html
    );
    assert!(
        rendered.html.contains("href=\"mailto:a@b.example\""),
        "{}",
        rendered.html
    );
}

/// One vector file for this policy and its browser port (board_url_policy.js),
/// whose Node suite asserts the same lists.
#[test]
fn url_policy_matches_shared_browser_vectors() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(
        "../assets/tests/support/board_url_policy_vectors.json"
    ))
    .expect("URL policy vectors are JSON");
    for (key, expected) in [("denied", false), ("allowed", true)] {
        let urls = vectors[key].as_array().expect("vector list");
        assert!(urls.len() > 10, "{key} vectors missing");
        for url in urls {
            let url = url.as_str().expect("vector string");
            assert_eq!(allowed_url(url), expected, "{key}: {url:?}");
        }
    }
}
