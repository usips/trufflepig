use super::fixture::{
    IndexedFixture, byte_span, method_id, no_internal_markers_or_synthetic_definitions,
    occurrences, relationships,
};

#[test]
fn parent_method_lookup_skips_layers_and_follows_ordinary_ancestors() {
    let root = r#"<?php
namespace Fixture;
class Root {
    public function run(): void {}
    public function ancestorOnly(): void {}
}
"#;
    let base = r#"<?php
namespace Fixture;
abstract class Base extends Root {
    public function run(): void {}
    private function privateStop(): void {}
    abstract public function abstractStop(): void;
}
"#;
    let unrelated = r#"<?php
namespace Unrelated;
class Methods {
    public function privateStop(): void {}
    public function abstractStop(): void {}
    public function traitStop(): void {}
}
"#;
    let layer_a = r#"<?php namespace AddonA\Fixture; class Widget extends XFCP_Widget {}"#;
    let layer_b = r#"<?php
namespace AddonB\Fixture;
class Widget extends XFCP_Widget {
    public function dispatch(): void {
        parent::run();
        parent::ancestorOnly();
        parent::privateStop();
        parent::abstractStop();
    }
}
"#;
    let xml_path = "src/addons/Methods/_data/class_extensions.xml";
    let tag_a = r#"<extension from_class="Fixture\Base" to_class="AddonA\Fixture\Widget" active="1" execute_order="10"/>"#;
    let tag_b = r#"<extension from_class="Fixture\Base" to_class="AddonB\Fixture\Widget" active="1" execute_order="20"/>"#;
    let xml = format!("<class_extensions>{tag_a}{tag_b}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/Fixture/Root.php", root),
        ("src/Fixture/Base.php", base),
        ("src/Unrelated/Methods.php", unrelated),
        ("src/addons/A/Widget.php", layer_a),
        ("src/addons/B/Widget.php", layer_b),
        (xml_path, &xml),
    ]);

    let caller = method_id(&fixture, r"AddonB\Fixture\Widget", "dispatch").unwrap();
    let run = method_id(&fixture, r"Fixture\Base", "run").unwrap();
    let ancestor = method_id(&fixture, r"Fixture\Root", "ancestorOnly").unwrap();
    let mut calls = occurrences(
        &fixture,
        "src/addons/B/Widget.php",
        "call",
        "php_parent_call_candidate",
    );
    calls.extend(occurrences(
        &fixture,
        "src/addons/B/Widget.php",
        "call",
        "php_parent_call_unresolved",
    ));
    let call_by_name = |name: &str| {
        calls
            .iter()
            .find(|occurrence| occurrence.name == name)
            .unwrap_or_else(|| panic!("missing parent call {name}: {calls:?}"))
    };

    for (name, expected) in [("run", run), ("ancestorOnly", ancestor)] {
        let call = call_by_name(name);
        assert_eq!(call.target, None, "PHP calls remain unresolved");
        assert_eq!(call.candidates, vec![expected], "{name}");
        assert_eq!(
            (call.start, call.end),
            byte_span(layer_b, name),
            "the candidate cites the exact PHP method token"
        );
    }
    for name in ["privateStop", "abstractStop"] {
        let call = call_by_name(name);
        assert_eq!(call.target, None);
        assert!(
            call.candidates.is_empty(),
            "{name} must stop at its barrier"
        );
    }

    let call_edges = relationships(&fixture, "php_parent_call_candidate");
    for (name, target) in [("run", run), ("ancestorOnly", ancestor)] {
        let span = byte_span(layer_b, name);
        assert!(call_edges.iter().any(|edge| {
            edge.source == caller && edge.target == Some(target) && (edge.start, edge.end) == span
        }));
    }
    for name in ["privateStop", "abstractStop"] {
        let span = byte_span(layer_b, name);
        assert!(
            !call_edges
                .iter()
                .any(|edge| { edge.source == caller && (edge.start, edge.end) == span })
        );
    }
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn parent_method_lookup_keeps_equal_priority_layers_as_candidates() {
    let base = r#"<?php namespace Fixture; class Base { public function run(): void {} }"#;
    let layer_a = r#"<?php namespace AddonA\Fixture; class Widget extends XFCP_Widget { public function run(): void { parent::run(); } }"#;
    let layer_b = r#"<?php namespace AddonB\Fixture; class Widget extends XFCP_Widget { public function run(): void { parent::run(); } }"#;
    let xml_path = "src/addons/Tied/_data/class_extensions.xml";
    let tag_a = r#"<extension from_class="Fixture\Base" to_class="AddonA\Fixture\Widget" active="1" execute_order="10"/>"#;
    let tag_b = r#"<extension from_class="Fixture\Base" to_class="AddonB\Fixture\Widget" active="1" execute_order="10"/>"#;
    let xml = format!("<class_extensions>{tag_b}{tag_a}</class_extensions>");
    let fixture = IndexedFixture::new(&[
        ("src/Fixture/Base.php", base),
        ("src/addons/A/Widget.php", layer_a),
        ("src/addons/B/Widget.php", layer_b),
        (xml_path, &xml),
    ]);

    let base_run = method_id(&fixture, r"Fixture\Base", "run").unwrap();
    let a_run = method_id(&fixture, r"AddonA\Fixture\Widget", "run").unwrap();
    let b_run = method_id(&fixture, r"AddonB\Fixture\Widget", "run").unwrap();
    let expected_a = sorted([base_run, b_run]);
    let expected_b = sorted([base_run, a_run]);
    let calls_a = occurrences(
        &fixture,
        "src/addons/A/Widget.php",
        "call",
        "php_parent_call_candidate",
    );
    let calls_b = occurrences(
        &fixture,
        "src/addons/B/Widget.php",
        "call",
        "php_parent_call_candidate",
    );
    assert_eq!(calls_a.len(), 1);
    assert_eq!(calls_b.len(), 1);
    assert_eq!(calls_a[0].target, None);
    assert_eq!(calls_a[0].candidates, expected_a);
    assert_eq!(calls_b[0].target, None);
    assert_eq!(calls_b[0].candidates, expected_b);
    no_internal_markers_or_synthetic_definitions(&fixture);
}

fn sorted<const N: usize>(mut ids: [i64; N]) -> Vec<i64> {
    ids.sort_unstable();
    ids.into()
}
