use super::fixture::{
    IndexedFixture, XF_ALIAS_SOURCE, byte_span, class_id, method_id,
    no_internal_markers_or_synthetic_definitions, occurrences, placeholder_occurrences,
    relationships,
};

#[test]
fn indexed_three_layer_chain_uses_execute_order_and_full_xml_spans() {
    let base = r#"<?php
namespace XF\Permission;
class Builder { public function run(): string { return 'base'; } }
"#;
    let layer_a = r#"<?php
namespace AddonA\Permission;
class Builder extends XFCP_Builder { public function run(): string { return parent::run(); } }
"#;
    let layer_b = r#"<?php
namespace AddonB\Permission;
class Builder extends XFCP_Builder { public function run(): string { return parent::run(); } }
"#;
    let layer_c = r#"<?php
namespace AddonC\Permission;
class Builder extends XFCP_Builder { public function run(): string { return parent::run(); } }
"#;
    let xml_path = "src/addons/Chain/_data/class_extensions.xml";
    let tag_c = r#"<extension from_class="XF\Permission\Builder" to_class="AddonC\Permission\Builder" active="1" execute_order="30"/>"#;
    let tag_a = r#"<extension from_class="XF\Permission\Builder" to_class="AddonA\Permission\Builder" active="1" execute_order="10"/>"#;
    let tag_b = r#"<extension from_class="XF\Permission\Builder" to_class="AddonB\Permission\Builder" active="1" execute_order="20"/>"#;
    let xml = format!("<class_extensions>{tag_c}{tag_a}{tag_b}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/XF/Permission/Builder.php", base),
        ("src/addons/A/Builder.php", layer_a),
        ("src/addons/B/Builder.php", layer_b),
        ("src/addons/C/Builder.php", layer_c),
        (xml_path, &xml),
    ]);

    let base_id = class_id(&fixture, r"XF\Permission\Builder");
    let a_id = class_id(&fixture, r"AddonA\Permission\Builder");
    let b_id = class_id(&fixture, r"AddonB\Permission\Builder");
    let c_id = class_id(&fixture, r"AddonC\Permission\Builder");
    let edges = relationships(&fixture, "framework_parent_candidate");
    assert_eq!(
        edges
            .iter()
            .map(|edge| (edge.source, edge.target))
            .collect::<Vec<_>>(),
        vec![
            (a_id, Some(base_id)),
            (b_id, Some(a_id)),
            (c_id, Some(b_id))
        ],
        "the XML order is C, A, B but execute_order must produce Base <- A <- B <- C"
    );
    assert_eq!(
        edges
            .iter()
            .map(|edge| (edge.start, edge.end))
            .collect::<Vec<_>>(),
        vec![
            byte_span(&xml, tag_a),
            byte_span(&xml, tag_b),
            byte_span(&xml, tag_c)
        ],
        "each projected parent keeps the complete XML element as evidence"
    );

    for (path, source) in [
        ("src/addons/A/Builder.php", layer_a),
        ("src/addons/B/Builder.php", layer_b),
        ("src/addons/C/Builder.php", layer_c),
    ] {
        let placeholders = placeholder_occurrences(&fixture, path);
        assert_eq!(placeholders.len(), 1, "{path}");
        assert_eq!(
            placeholders[0].target, None,
            "PHP candidates stay unresolved"
        );
        assert!(placeholders[0].candidates.len() <= 64);
        assert_eq!(
            (placeholders[0].start, placeholders[0].end),
            byte_span(source, "XFCP_Builder"),
            "the proxy occurrence span uses original PHP bytes"
        );
    }
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn literal_base_alias_keeps_the_raw_proxy_class_identity() {
    let base = r#"<?php
namespace XF\Pub\Controller;
class ForumController {}
"#;
    let implementation = r#"<?php
namespace Vendor\Addon\XF\Pub\Controller;
class Forum extends \Vendor\Addon\XF\Pub\Controller\XFCP_Forum {}
"#;
    let xml_path = "src/addons/Vendor/Addon/_data/class_extensions.xml";
    let tag = r#"<extension from_class="XF\Pub\Controller\Forum" to_class="Vendor\Addon\XF\Pub\Controller\Forum" active="1" execute_order="10"/>"#;
    let xml = format!("<class_extensions>{tag}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/XF/Pub/Controller/ForumController.php", base),
        ("src/addons/Vendor/Addon/Forum.php", implementation),
        ("src/XF.php", XF_ALIAS_SOURCE),
        (xml_path, &xml),
    ]);

    let canonical_base = class_id(&fixture, r"XF\Pub\Controller\ForumController");
    let implementation_id = class_id(&fixture, r"Vendor\Addon\XF\Pub\Controller\Forum");
    let edge = relationships(&fixture, "framework_parent_candidate");
    assert_eq!(edge.len(), 1);
    assert_eq!(edge[0].source, implementation_id);
    assert_eq!(edge[0].target, Some(canonical_base));
    assert_eq!((edge[0].start, edge[0].end), byte_span(&xml, tag));

    let placeholders = placeholder_occurrences(&fixture, "src/addons/Vendor/Addon/Forum.php");
    assert_eq!(placeholders.len(), 1);
    assert_eq!(
        placeholders[0].name,
        r"\Vendor\Addon\XF\Pub\Controller\XFCP_Forum"
    );
    assert_eq!(placeholders[0].target, None);
    assert_eq!(placeholders[0].candidates, vec![canonical_base]);
    assert_eq!(
        (placeholders[0].start, placeholders[0].end),
        byte_span(
            implementation,
            r"\Vendor\Addon\XF\Pub\Controller\XFCP_Forum"
        )
    );
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn equal_order_extensions_remain_tied_candidates_without_a_winner() {
    let base = r#"<?php namespace XF\Entity; class User { public function getStructure() {} }"#;
    let extension_a = r#"<?php namespace AddonA\XF\Entity; class User extends XFCP_User { public function getStructure() { return parent::getStructure(); } }"#;
    let extension_b = r#"<?php namespace AddonB\XF\Entity; class User extends XFCP_User { public function getStructure() { return parent::getStructure(); } }"#;
    let extension_c = r#"<?php namespace AddonC\XF\Entity; class User extends XFCP_User { public function getStructure() { return parent::getStructure(); } }"#;
    let xml_path = "src/addons/Tied/_data/class_extensions.xml";
    let tag_c = r#"<extension from_class="XF\Entity\User" to_class="AddonC\XF\Entity\User" active="1" execute_order="20"/>"#;
    let tag_a = r#"<extension from_class="XF\Entity\User" to_class="AddonA\XF\Entity\User" active="1" execute_order="10"/>"#;
    let tag_b = r#"<extension from_class="XF\Entity\User" to_class="AddonB\XF\Entity\User" active="1" execute_order="10"/>"#;
    let xml = format!("<class_extensions>{tag_c}{tag_b}{tag_a}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/XF/Entity/User.php", base),
        ("src/addons/A/User.php", extension_a),
        ("src/addons/B/User.php", extension_b),
        ("src/addons/C/User.php", extension_c),
        (xml_path, &xml),
    ]);

    let base_id = class_id(&fixture, r"XF\Entity\User");
    let a_id = class_id(&fixture, r"AddonA\XF\Entity\User");
    let b_id = class_id(&fixture, r"AddonB\XF\Entity\User");
    let c_id = class_id(&fixture, r"AddonC\XF\Entity\User");
    let edges = relationships(&fixture, "framework_parent_candidate");
    let targets = |source| {
        let mut targets = edges
            .iter()
            .filter(|edge| edge.source == source)
            .filter_map(|edge| edge.target)
            .collect::<Vec<_>>();
        targets.sort_unstable();
        targets
    };
    let mut a_targets = vec![base_id, b_id];
    a_targets.sort_unstable();
    let mut b_targets = vec![base_id, a_id];
    b_targets.sort_unstable();
    let mut c_targets = vec![a_id, b_id];
    c_targets.sort_unstable();
    assert_eq!(targets(a_id), a_targets);
    assert_eq!(targets(b_id), b_targets);
    assert_eq!(targets(c_id), c_targets);
    assert!(edges.iter().any(|edge| edge.provenance.contains("tie")));
    assert_eq!(
        edges.len(),
        6,
        "no candidate may silently win the order-10 tie"
    );

    let base_method = method_id(&fixture, r"XF\Entity\User", "getStructure").unwrap();
    let a_method = method_id(&fixture, r"AddonA\XF\Entity\User", "getStructure").unwrap();
    let b_method = method_id(&fixture, r"AddonB\XF\Entity\User", "getStructure").unwrap();
    for (path, expected) in [
        ("src/addons/A/User.php", vec![base_method, b_method]),
        ("src/addons/B/User.php", vec![base_method, a_method]),
        ("src/addons/C/User.php", vec![a_method, b_method]),
    ] {
        let calls = occurrences(&fixture, path, "call", "php_parent_call_candidate");
        let call = calls
            .iter()
            .find(|call| call.name == "getStructure")
            .unwrap_or_else(|| panic!("missing parent::getStructure call in {path}"));
        let mut actual = call.candidates.clone();
        actual.sort_unstable();
        let mut expected = expected;
        expected.sort_unstable();
        assert_eq!(call.target, None, "PHP calls remain unresolved");
        assert_eq!(
            actual, expected,
            "parent::getStructure in {path} retains every tied predecessor method"
        );
    }

    let placeholders = placeholder_occurrences(&fixture, "src/addons/A/User.php");
    assert_eq!(placeholders.len(), 1);
    assert_eq!(placeholders[0].target, None);
    assert_eq!(placeholders[0].candidates.len(), 2);
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn metadata_owner_does_not_filter_an_exact_class_in_another_addon() {
    let global_base = r#"<?php class GlobalBase {}"#;
    let implementation = r#"<?php
namespace AddonTwo;
class Implementation extends XFCP_Implementation {}
"#;
    let xml_path = "src/addons/AddonOne/_data/class_extensions.xml";
    let tag = r#"<extension from_class="GlobalBase" to_class="AddonTwo\Implementation" active="1" execute_order="10"/>"#;
    let xml = format!("<class_extensions>{tag}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/GlobalBase.php", global_base),
        ("src/addons/AddonTwo/Implementation.php", implementation),
        (xml_path, &xml),
    ]);

    let base_id = class_id(&fixture, "GlobalBase");
    let implementation_id = class_id(&fixture, r"AddonTwo\Implementation");
    let edges = relationships(&fixture, "framework_parent_candidate");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].source, implementation_id);
    assert_eq!(edges[0].target, Some(base_id));
    assert_eq!((edges[0].start, edges[0].end), byte_span(&xml, tag));

    let placeholders = placeholder_occurrences(&fixture, "src/addons/AddonTwo/Implementation.php");
    assert_eq!(placeholders.len(), 1);
    assert_eq!(placeholders[0].target, None);
    assert_eq!(placeholders[0].candidates, vec![base_id]);
    assert_eq!(
        (placeholders[0].start, placeholders[0].end),
        byte_span(implementation, "XFCP_Implementation")
    );
    no_internal_markers_or_synthetic_definitions(&fixture);
}
