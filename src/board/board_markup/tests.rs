use super::headings;

#[test]
fn headings_follow_formatted_visible_text_and_commonmark_boundaries() {
    let body = "# **Build** `API` &amp; [deploy](https://example.test) ###\n\n\
                Setext *section*\n----------------\n\n\
                ```\n# fenced\n```\n\n    # indented\n\n#not-a-heading\n";
    let headings = headings(body);
    assert_eq!(headings.len(), 2);
    assert_eq!(headings[0].level, 1);
    assert_eq!(headings[0].title, "Build API & deploy");
    assert_eq!(headings[0].anchor, "plan-build-api-deploy");
    assert_eq!(headings[1].level, 2);
    assert_eq!(headings[1].title, "Setext section");
}

#[test]
fn duplicate_and_colliding_heading_anchors_are_unique_and_stable() {
    let body = "# Intro\n# Intro\n# Intro-2\n# Intro\n# !!!\n#\n";
    let first = headings(body);
    assert_eq!(first, headings(body));
    assert_eq!(
        first
            .iter()
            .map(|heading| heading.anchor.as_str())
            .collect::<Vec<_>>(),
        [
            "plan-intro",
            "plan-intro-2",
            "plan-intro-2-2",
            "plan-intro-3",
            "plan-section",
            "plan-section-2",
        ]
    );
    assert_eq!(first[0].title, first[1].title);
    assert_eq!(first[5].title, "");
}

#[test]
fn headings_include_escaped_html_and_omit_suppressed_image_alt() {
    let headings =
        headings("# Before ![secret **alt**](data:image/png;base64,AAAA) after <b>tag</b>\n");
    assert_eq!(headings[0].title, "Before after <b>tag</b>");
    assert_eq!(headings[0].anchor, "plan-before-after-b-tag-b");
}
