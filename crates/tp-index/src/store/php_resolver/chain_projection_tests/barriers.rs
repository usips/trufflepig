use super::fixture::{
    IndexedFixture, byte_span, class_id, definition_count, definition_id,
    no_internal_markers_or_synthetic_definitions, placeholder_occurrences, relationships,
};

#[test]
fn missing_implementation_blocks_splicing_but_keeps_the_next_known_layer() {
    let base = r#"<?php namespace XF\Widget; class Base {}"#;
    let layer_b = r#"<?php namespace AddonB\Widget; class B extends XFCP_B {}"#;
    let layer_c = r#"<?php namespace AddonC\Widget; class C extends XFCP_C {}"#;
    let path = "src/addons/Chain/_data/class_extensions.xml";
    let tag_a = r#"<extension from_class="XF\Widget\Base" to_class="AddonA\Widget\A" active="1" execute_order="10"/>"#;
    let tag_b = r#"<extension from_class="XF\Widget\Base" to_class="AddonB\Widget\B" active="1" execute_order="20"/>"#;
    let tag_c = r#"<extension from_class="XF\Widget\Base" to_class="AddonC\Widget\C" active="1" execute_order="30"/>"#;
    let xml = format!("<class_extensions>{tag_c}{tag_b}{tag_a}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/XF/Widget/Base.php", base),
        ("src/addons/B/B.php", layer_b),
        ("src/addons/C/C.php", layer_c),
        (path, &xml),
    ]);

    let b_id = class_id(&fixture, r"AddonB\Widget\B");
    let c_id = class_id(&fixture, r"AddonC\Widget\C");
    let base_id = class_id(&fixture, r"XF\Widget\Base");
    let edges = relationships(&fixture, "framework_parent_candidate");
    assert_eq!(edges.len(), 1, "the valid C-to-B segment survives the gap");
    assert_eq!((edges[0].source, edges[0].target), (c_id, Some(b_id)));
    assert_ne!(
        edges[0].target,
        Some(base_id),
        "B must not splice across missing A"
    );
    assert_eq!((edges[0].start, edges[0].end), byte_span(&xml, tag_c));

    let issues = relationships(&fixture, "inheritance_issue");
    assert!(issues.iter().any(|issue| {
        issue.source == b_id
            && issue.target.is_none()
            && (issue.start, issue.end) == byte_span(&xml, tag_b)
    }));
    let b_placeholder = placeholder_occurrences(&fixture, "src/addons/B/B.php");
    assert_eq!(b_placeholder.len(), 1);
    assert_eq!(b_placeholder[0].target, None);
    assert!(b_placeholder[0].candidates.is_empty());
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn unknown_active_order_duplicate_and_reused_proxy_rows_block_composition() {
    let base = r#"<?php namespace XF\Widget; class Base {}"#;
    let first = r#"<?php namespace AddonA\Widget; class A extends XFCP_A {}"#;
    let second = r#"<?php namespace AddonB\Widget; class B extends XFCP_B {}"#;
    let unknown_path = "src/addons/Unknown/_data/class_extensions.xml";
    let unknown = r#"<class_extensions><extension from_class="XF\Widget\Base" to_class="AddonA\Widget\A" active="1" execute_order="bad"/><extension from_class="XF\Widget\Base" to_class="AddonB\Widget\B" active="1" execute_order="20"/></class_extensions>"#;
    let unknown_fixture = IndexedFixture::new(&[
        ("src/XF/Widget/Base.php", base),
        ("src/addons/A/A.php", first),
        ("src/addons/B/B.php", second),
        (unknown_path, unknown),
    ]);
    let unknown_edges = relationships(&unknown_fixture, "framework_parent_candidate");
    assert!(
        unknown_edges.is_empty(),
        "an unknown layer order invalidates composition"
    );
    let unknown_issues = relationships(&unknown_fixture, "inheritance_issue");
    assert!(!unknown_issues.is_empty());
    assert!(unknown_issues.iter().all(|issue| issue.target.is_none()));
    no_internal_markers_or_synthetic_definitions(&unknown_fixture);

    let negative_order = r#"<class_extensions><extension from_class="XF\Widget\Base" to_class="AddonA\Widget\A" active="1" execute_order="-1"/><extension from_class="XF\Widget\Base" to_class="AddonB\Widget\B" active="1" execute_order="20"/></class_extensions>"#;
    let negative_order_fixture = IndexedFixture::new(&[
        ("src/XF/Widget/Base.php", base),
        ("src/addons/A/A.php", first),
        ("src/addons/B/B.php", second),
        (
            "src/addons/NegativeOrder/_data/class_extensions.xml",
            negative_order,
        ),
    ]);
    assert!(relationships(&negative_order_fixture, "framework_parent_candidate").is_empty());
    let order_issues = relationships(&negative_order_fixture, "inheritance_issue");
    assert!(!order_issues.is_empty());
    assert!(order_issues.iter().all(|issue| issue.target.is_none()));
    no_internal_markers_or_synthetic_definitions(&negative_order_fixture);

    let duplicate_path = "src/addons/Duplicate/_data/class_extensions.xml";
    let duplicate_tag = r#"<extension from_class="XF\Widget\Base" to_class="AddonA\Widget\A" active="1" execute_order="10"/>"#;
    let duplicate_xml =
        format!("<class_extensions>{duplicate_tag}{duplicate_tag}</class_extensions>");
    let duplicate_fixture = IndexedFixture::new(&[
        ("src/XF/Widget/Base.php", base),
        ("src/addons/A/A.php", first),
        (duplicate_path, &duplicate_xml),
    ]);
    assert!(relationships(&duplicate_fixture, "framework_parent_candidate").is_empty());
    let duplicate_issues = relationships(&duplicate_fixture, "inheritance_issue");
    assert!(!duplicate_issues.is_empty());
    assert!(duplicate_issues.iter().all(|issue| issue.target.is_none()));
    no_internal_markers_or_synthetic_definitions(&duplicate_fixture);

    let invalid_active = r#"<class_extensions><extension from_class="XF\Widget\Base" to_class="AddonA\Widget\A" active="enabled" execute_order="10"/><extension from_class="XF\Widget\Base" to_class="AddonB\Widget\B" active="1" execute_order="20"/></class_extensions>"#;
    let invalid_active_fixture = IndexedFixture::new(&[
        ("src/XF/Widget/Base.php", base),
        ("src/addons/A/A.php", first),
        ("src/addons/B/B.php", second),
        (
            "src/addons/InvalidActive/_data/class_extensions.xml",
            invalid_active,
        ),
    ]);
    assert!(relationships(&invalid_active_fixture, "framework_parent_candidate").is_empty());
    let active_issues = relationships(&invalid_active_fixture, "inheritance_issue");
    assert!(!active_issues.is_empty());
    assert!(active_issues.iter().all(|issue| issue.target.is_none()));
    no_internal_markers_or_synthetic_definitions(&invalid_active_fixture);

    let string_false_active = r#"<class_extensions><extension from_class="XF\Widget\Base" to_class="AddonA\Widget\A" active="false" execute_order="10"/><extension from_class="XF\Widget\Base" to_class="AddonB\Widget\B" active="1" execute_order="20"/></class_extensions>"#;
    let string_false_fixture = IndexedFixture::new(&[
        ("src/XF/Widget/Base.php", base),
        ("src/addons/A/A.php", first),
        ("src/addons/B/B.php", second),
        (
            "src/addons/StringFalse/_data/class_extensions.xml",
            string_false_active,
        ),
    ]);
    assert!(relationships(&string_false_fixture, "framework_parent_candidate").is_empty());
    let false_issues = relationships(&string_false_fixture, "inheritance_issue");
    assert!(!false_issues.is_empty());
    assert!(false_issues.iter().all(|issue| issue.target.is_none()));
    no_internal_markers_or_synthetic_definitions(&string_false_fixture);

    let reused_path = "src/addons/Reused/_data/class_extensions.xml";
    let reused = r#"<class_extensions><extension from_class="XF\Widget\Base" to_class="AddonA\Widget\A" active="1" execute_order="10"/><extension from_class="Other\Widget\Base" to_class="AddonA\Widget\A" active="1" execute_order="10"/></class_extensions>"#;
    let other_base = r#"<?php namespace Other\Widget; class Base {}"#;
    let reused_fixture = IndexedFixture::new(&[
        ("src/XF/Widget/Base.php", base),
        ("src/Other/Widget/Base.php", other_base),
        ("src/addons/A/A.php", first),
        (reused_path, reused),
    ]);
    assert!(relationships(&reused_fixture, "framework_parent_candidate").is_empty());
    let reused_issues = relationships(&reused_fixture, "inheritance_issue");
    assert!(!reused_issues.is_empty());
    assert!(reused_issues.iter().all(|issue| issue.target.is_none()));
    no_internal_markers_or_synthetic_definitions(&reused_fixture);
}

#[test]
fn malformed_xml_discards_all_extension_facts_from_that_file() {
    let base = r#"<?php namespace XF\Widget; class Base {}"#;
    let implementation = r#"<?php namespace AddonA\Widget; class A extends XFCP_A {}"#;
    let addon_path = "src/addons/Broken/Addon/addon.json";
    let path = "src/addons/Broken/Addon/_data/class_extensions.xml";
    let manifest = r#"{"legacyId":"Broken/Addon","require":{}}"#;
    let tag = r#"<extension from_class="XF\Widget\Base" to_class="AddonA\Widget\A" active="1" execute_order="10"/>"#;
    let xml = format!("<class_extensions>{tag}</not_class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/XF/Widget/Base.php", base),
        ("src/addons/A/A.php", implementation),
        (addon_path, manifest),
        (path, &xml),
    ]);

    assert_eq!(
        definition_count(&fixture, path, "xenforo_class_extension"),
        0
    );
    assert!(relationships(&fixture, "framework_parent_candidate").is_empty());
    assert!(relationships(&fixture, "xenforo_class_extension_candidate").is_empty());
    let addon_id = definition_id(&fixture, addon_path, "addon", "Broken/Addon").unwrap();
    assert!(
        relationships(&fixture, "inheritance_issue")
            .iter()
            .any(|issue| {
                issue.source == addon_id
                    && issue.target.is_none()
                    && (issue.start, issue.end) == (0, xml.len() as i64)
            })
    );
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn proxy_leaf_matches_require_the_expected_namespace() {
    let base = r#"<?php namespace XF\Widget; class Base {}"#;
    let global_impl = r#"<?php namespace AddonA\Widget; class GlobalA extends \XFCP_A {}"#;
    let imported_impl = r#"<?php
namespace AddonA\Widget;
use Other\XFCP_B;
class ImportedB extends XFCP_B {}
"#;
    let path = "src/addons/GlobalProxy/_data/class_extensions.xml";
    let tag_a = r#"<extension from_class="XF\Widget\Base" to_class="AddonA\Widget\GlobalA" active="1" execute_order="10"/>"#;
    let tag_b = r#"<extension from_class="XF\Widget\Base" to_class="AddonA\Widget\ImportedB" active="1" execute_order="20"/>"#;
    let xml = format!("<class_extensions>{tag_a}{tag_b}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/XF/Widget/Base.php", base),
        ("src/addons/A/GlobalA.php", global_impl),
        ("src/addons/B/ImportedB.php", imported_impl),
        (path, &xml),
    ]);

    let global_proxy = placeholder_occurrences(&fixture, "src/addons/A/GlobalA.php");
    assert_eq!(global_proxy.len(), 1);
    assert_eq!(global_proxy[0].name, r"\XFCP_A");
    assert_eq!(global_proxy[0].target, None);
    assert!(global_proxy[0].candidates.is_empty());

    let imported_proxy = placeholder_occurrences(&fixture, "src/addons/B/ImportedB.php");
    assert_eq!(imported_proxy.len(), 1);
    assert_eq!(imported_proxy[0].name, r"Other\XFCP_B");
    assert_eq!(imported_proxy[0].target, None);
    assert!(
        imported_proxy[0].candidates.is_empty(),
        "the import keeps Other\\XFCP_B as its exact target instead of borrowing the AddonA\\Widget leaf"
    );
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn ordinary_parent_cycle_is_published_as_candidates_and_an_issue() {
    let cycle = r#"<?php
namespace Cycle;
class First extends Second { public function run(): void { parent::missing(); } }
class Second extends First {}
"#;
    let fixture = IndexedFixture::new(&[("src/Cycle.php", cycle)]);
    let first = class_id(&fixture, r"Cycle\First");
    let second = class_id(&fixture, r"Cycle\Second");
    let edges = relationships(&fixture, "php_extends_candidate");
    assert_eq!(
        edges
            .iter()
            .map(|edge| (edge.source, edge.target))
            .collect::<Vec<_>>(),
        vec![(first, Some(second)), (second, Some(first))]
    );
    let issues = relationships(&fixture, "inheritance_issue");
    assert!(issues.iter().any(|issue| {
        issue.target.is_none() && issue.provenance.to_ascii_lowercase().contains("cycle")
    }));
    no_internal_markers_or_synthetic_definitions(&fixture);
}
