use super::{allowed_url, render};
use crate::board::board_markup::headings;

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
    assert!(rendered.html.contains("href=\"../plan?x=1&amp;y=2\""));
    assert!(rendered.html.contains("href=\"HTTPS://example.test/\""));
    assert!(rendered.html.contains("href=\"mailto:owner@example.test\""));
    assert!(rendered.html.contains("script"));
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

#[test]
fn url_policy_rejects_ambiguous_schemes_and_browser_normalization() {
    for url in [
        "javascript:alert(1)",
        "data:text/html,boom",
        "vbscript:evil",
        "java\tscript:evil",
        "https://example.test/\n",
        " https://example.test/",
        "https:\\example.test/",
        "//example.test/",
        "https:example.test",
        "https:///example.test",
        ":evil",
        "1http:evil",
        "java%73cript:evil",
        "mailto:",
    ] {
        assert!(!allowed_url(url), "accepted {url:?}");
    }
    for url in [
        "http://example.test/x",
        "HTTPS://example.test/x",
        "mailto:owner@example.test",
        "/plan/one",
        "./javascript:filename",
        "../plan?next=javascript:ignored",
        "#plan-build-api",
        "?query=a:b",
        "",
    ] {
        assert!(allowed_url(url), "rejected {url:?}");
    }
}
