use super::{allowed_url, render};

fn assert_hosts_render_as_text(hosts: &[&str]) {
    for href in hosts {
        let rendered = render(&format!("[host]({href})"));
        assert!(
            !rendered.html.contains("href="),
            "{href}: {}",
            rendered.html
        );
        assert!(
            rendered.html.contains(">host<"),
            "{href}: {}",
            rendered.html
        );
        assert!(!allowed_url(href), "accepted {href:?}");
    }
}

#[test]
fn empty_hex_labels_in_hex_ipv4_hosts_render_as_text() {
    assert_hosts_render_as_text(&[
        "http://0x7f.0x.0x.1/",
        "http://0X7F.0X.0X.1/",
        "http://0x7f.%30x.%30X.1/",
    ]);
}

#[test]
fn empty_hex_labels_in_decimal_ipv4_hosts_render_as_text() {
    assert_hosts_render_as_text(&[
        "http://127.0x.0x.1/",
        "http://127.0X.0x.1/",
        "https://user@127.0x.0X.1:7341/page",
    ]);
}

#[test]
fn empty_hex_single_integer_hosts_render_as_text() {
    assert_hosts_render_as_text(&["http://0x/", "http://0X/", "https://%30X:7341/page"]);
}

#[test]
fn empty_hex_prefix_domains_keep_their_links() {
    for href in ["http://0x.example.test/", "http://0Xwidget.example.test/"] {
        assert!(allowed_url(href), "rejected {href:?}");
        let rendered = render(&format!("[host]({href})"));
        assert!(
            rendered.html.contains(&format!("href=\"{href}\"")),
            "{}",
            rendered.html
        );
    }
}
