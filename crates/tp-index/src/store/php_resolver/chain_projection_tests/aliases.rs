use super::fixture::{
    IndexedFixture, XF_ALIAS_SOURCE, byte_span, class_id,
    no_internal_markers_or_synthetic_definitions, occurrences, placeholder_occurrences,
    relationships,
};

#[test]
fn xenforo_literal_alias_is_local_to_metadata_and_proxy_imports_keep_raw_spans() {
    let raw_classes = r#"<?php
namespace Vendor\Controller;
class Base {}
class BaseController {}
class Child extends Base {}
"#;
    let xenforo_base = r#"<?php namespace XF\Pub\Controller; class ForumController {}"#;
    let extension = r#"<?php
namespace Vendor\Addon;
use Vendor\Addon\XFCP_Forum as ForumProxy;
class Forum extends ForumProxy {}
"#;
    let xml_path = "src/addons/Vendor/Addon/_data/class_extensions.xml";
    let tag = r#"<extension from_class="XF\Pub\Controller\Forum" to_class="Vendor\Addon\Forum" active="1" execute_order="10"/>"#;
    let xml = format!("<class_extensions>{tag}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/Vendor/Controller/classes.php", raw_classes),
        ("src/XF/Pub/Controller/ForumController.php", xenforo_base),
        ("src/addons/Vendor/Addon/Forum.php", extension),
        ("src/XF.php", XF_ALIAS_SOURCE),
        (xml_path, &xml),
    ]);

    let child_id = class_id(&fixture, r"Vendor\Controller\Child");
    let raw_base_id = class_id(&fixture, r"Vendor\Controller\Base");
    let base_controller_id = class_id(&fixture, r"Vendor\Controller\BaseController");
    let ordinary = relationships(&fixture, "php_extends_candidate");
    let child_targets = ordinary
        .iter()
        .filter(|edge| edge.source == child_id)
        .filter_map(|edge| edge.target)
        .collect::<Vec<_>>();
    assert_eq!(child_targets, vec![raw_base_id]);
    assert_ne!(child_targets[0], base_controller_id);

    let forum_id = class_id(&fixture, r"Vendor\Addon\Forum");
    let forum_controller_id = class_id(&fixture, r"XF\Pub\Controller\ForumController");
    let framework = relationships(&fixture, "framework_parent_candidate");
    assert_eq!(framework.len(), 1);
    assert_eq!(framework[0].source, forum_id);
    assert_eq!(framework[0].target, Some(forum_controller_id));
    assert_eq!((framework[0].start, framework[0].end), byte_span(&xml, tag));

    let proxies = placeholder_occurrences(&fixture, "src/addons/Vendor/Addon/Forum.php");
    assert_eq!(proxies.len(), 1);
    assert_eq!(proxies[0].name, r"Vendor\Addon\XFCP_Forum");
    assert_eq!(proxies[0].target, None);
    assert_eq!(proxies[0].candidates, vec![forum_controller_id]);
    let proxy_alias = "ForumProxy";
    let proxy_start = extension.rfind(proxy_alias).unwrap() as i64;
    assert_eq!(
        (proxies[0].start, proxies[0].end),
        (proxy_start, proxy_start + proxy_alias.len() as i64),
        "the imported proxy keeps the PHP use-site span rather than its canonical name span"
    );
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn alias_targets_reusing_one_implementation_block_the_affected_chain() {
    let base = r#"<?php namespace XF\Pub\Controller; class ForumController {}"#;
    let implementation = r#"<?php
namespace Vendor\Controller;
class ForumController extends XFCP_Forum {}
"#;
    let xml_path = "src/addons/Vendor/Controller/_data/class_extensions.xml";
    let tag_forum = r#"<extension from_class="XF\Pub\Controller\Forum" to_class="Vendor\Controller\Forum" active="1" execute_order="10"/>"#;
    let tag_forum_controller = r#"<extension from_class="XF\Pub\Controller\Forum" to_class="Vendor\Controller\ForumController" active="1" execute_order="20"/>"#;
    let xml = format!("<class_extensions>{tag_forum}{tag_forum_controller}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/XF/Pub/Controller/ForumController.php", base),
        (
            "src/addons/Vendor/Controller/ForumController.php",
            implementation,
        ),
        ("src/XF.php", XF_ALIAS_SOURCE),
        (xml_path, &xml),
    ]);

    let base_id = class_id(&fixture, r"XF\Pub\Controller\ForumController");
    let implementation_id = class_id(&fixture, r"Vendor\Controller\ForumController");
    let implementation_occurrences = occurrences(
        &fixture,
        xml_path,
        "xenforo_extension_implementation",
        "xenforo_class_extensions_xml",
    );
    assert_eq!(implementation_occurrences.len(), 2);
    assert!(implementation_occurrences.iter().all(|occurrence| {
        occurrence.target.is_none() && occurrence.candidates == vec![implementation_id]
    }));
    let base_occurrences = occurrences(
        &fixture,
        xml_path,
        "xenforo_extension_base",
        "xenforo_class_extensions_xml",
    );
    assert_eq!(base_occurrences.len(), 2);
    assert!(base_occurrences
        .iter()
        .all(|occurrence| occurrence.target.is_none() && occurrence.candidates == vec![base_id]));

    let framework = relationships(&fixture, "framework_parent_candidate");
    assert!(
        framework
            .iter()
            .all(|edge| edge.target != Some(edge.source))
    );
    assert!(framework.is_empty());
    let issues = relationships(&fixture, "inheritance_issue");
    for tag in [tag_forum, tag_forum_controller] {
        let span = byte_span(&xml, tag);
        assert!(
            issues
                .iter()
                .any(|issue| { issue.target.is_none() && (issue.start, issue.end) == span })
        );
    }

    let proxies =
        placeholder_occurrences(&fixture, "src/addons/Vendor/Controller/ForumController.php");
    assert_eq!(proxies.len(), 1);
    assert_eq!(proxies[0].target, None);
    assert!(proxies[0].candidates.is_empty());
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn self_registration_reports_an_issue_without_a_self_parent_edge() {
    let implementation = r#"<?php
namespace XF\Pub\Controller;
class ForumController extends XFCP_ForumController {}
"#;
    let xml_path = "src/addons/Self/_data/class_extensions.xml";
    let tag = r#"<extension from_class="XF\Pub\Controller\Forum" to_class="XF\Pub\Controller\ForumController" active="1" execute_order="10"/>"#;
    let xml = format!("<class_extensions>{tag}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/XF/Pub/Controller/ForumController.php", implementation),
        (xml_path, &xml),
    ]);

    let forum_id = class_id(&fixture, r"XF\Pub\Controller\ForumController");
    let framework = relationships(&fixture, "framework_parent_candidate");
    assert!(
        framework
            .iter()
            .all(|edge| edge.target != Some(edge.source))
    );
    assert!(framework.is_empty());
    assert!(
        relationships(&fixture, "inheritance_issue")
            .iter()
            .any(|issue| {
                issue.source == forum_id
                    && issue.target.is_none()
                    && (issue.start, issue.end) == byte_span(&xml, tag)
            })
    );
    let proxies = placeholder_occurrences(&fixture, "src/XF/Pub/Controller/ForumController.php");
    assert_eq!(proxies.len(), 1);
    assert_eq!(proxies[0].target, None);
    assert!(proxies[0].candidates.is_empty());
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn unresolved_alias_metadata_keeps_exact_implementation_candidates() {
    let xf = r#"<?php
class XF
{
    public static function getAliasableNamespaces(): array
    {
        return static::buildRules();
    }

    public static function buildRules(): array
    {
        return ['Controller'];
    }
}
"#;
    let base = r#"<?php namespace Unknown; class Base {}"#;
    let implementation = r#"<?php
namespace Known;
class Extension extends XFCP_Extension {}
"#;
    let xml_path = "src/addons/Known/_data/class_extensions.xml";
    let tag = r#"<extension from_class="Unknown\Base" to_class="Known\Extension" active="1" execute_order="10"/>"#;
    let xml = format!("<class_extensions>{tag}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/XF.php", xf),
        ("src/Unknown/Base.php", base),
        ("src/addons/Known/Extension.php", implementation),
        (xml_path, &xml),
    ]);

    let implementation_id = class_id(&fixture, r"Known\Extension");
    let implementation_occurrences = occurrences(
        &fixture,
        xml_path,
        "xenforo_extension_implementation",
        "xenforo_class_extensions_xml",
    );
    assert_eq!(implementation_occurrences.len(), 1);
    assert_eq!(implementation_occurrences[0].name, r"Known\Extension");
    assert_eq!(implementation_occurrences[0].target, None);
    assert_eq!(
        implementation_occurrences[0].candidates,
        vec![implementation_id]
    );
    assert_eq!(
        (
            implementation_occurrences[0].start,
            implementation_occurrences[0].end
        ),
        byte_span(&xml, r"Known\Extension")
    );

    let framework = relationships(&fixture, "framework_parent_candidate");
    assert!(framework.is_empty());
    assert!(
        relationships(&fixture, "inheritance_issue")
            .iter()
            .any(|issue| {
                issue.target.is_none() && (issue.start, issue.end) == byte_span(&xml, tag)
            })
    );
    no_internal_markers_or_synthetic_definitions(&fixture);
}
