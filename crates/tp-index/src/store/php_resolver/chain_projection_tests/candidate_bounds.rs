use super::fixture::{
    IndexedFixture, no_internal_markers_or_synthetic_definitions, placeholder_occurrences,
    relationships,
};

#[test]
fn over_limit_extension_chain_is_rejected_as_a_whole() {
    let extension_count = 65;
    let mut owned_files = vec![(
        "src/XF/Base.php".to_owned(),
        "<?php namespace XF; class Base {}".to_owned(),
    )];
    let mut xml = String::from("<class_extensions>");
    for index in 0..extension_count {
        let namespace = format!("Addon\\Layer{index:02}");
        let short_name = format!("Impl{index:02}");
        let fqcn = format!("{namespace}\\{short_name}");
        let path = format!("src/addons/Layer{index:02}/{short_name}.php");
        let source = format!(
            "<?php namespace {namespace}; class {short_name} extends XFCP_{short_name} {{}}"
        );
        owned_files.push((path, source));
        xml.push_str(&format!(
            "<extension from_class=\"XF\\Base\" to_class=\"{fqcn}\" active=\"1\" execute_order=\"10\"/>"
        ));
    }
    xml.push_str("</class_extensions>");
    owned_files.push(("src/addons/Tied/_data/class_extensions.xml".to_owned(), xml));
    let borrowed_files = owned_files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let fixture = IndexedFixture::new(&borrowed_files);

    assert!(relationships(&fixture, "framework_parent_candidate").is_empty());
    let issues = relationships(&fixture, "inheritance_issue");
    assert!(issues.iter().any(|issue| {
        issue.target.is_none() && issue.provenance.to_ascii_lowercase().contains("limit")
    }));
    let first_proxy = placeholder_occurrences(&fixture, "src/addons/Layer00/Impl00.php");
    assert_eq!(first_proxy.len(), 1);
    assert_eq!(first_proxy[0].target, None);
    assert!(first_proxy[0].candidates.is_empty());
    no_internal_markers_or_synthetic_definitions(&fixture);
}

#[test]
fn duplicate_base_identity_blocks_proxy_candidates() {
    let duplicate_base = "<?php namespace XF; class Base {}";
    let implementation = "<?php namespace Addon; class Impl extends XFCP_Impl {}";
    let xml = r#"<class_extensions><extension from_class="XF\Base" to_class="Addon\Impl" active="1" execute_order="10"/></class_extensions>"#;
    let fixture = IndexedFixture::new(&[
        ("src/core/FirstBase.php", duplicate_base),
        ("src/core/SecondBase.php", duplicate_base),
        ("src/addons/Addon/Impl.php", implementation),
        ("src/addons/Addon/_data/class_extensions.xml", xml),
    ]);

    assert!(relationships(&fixture, "framework_parent_candidate").is_empty());
    let issues = relationships(&fixture, "inheritance_issue");
    assert!(!issues.is_empty());
    assert!(issues.iter().all(|issue| issue.target.is_none()));
    let proxy = placeholder_occurrences(&fixture, "src/addons/Addon/Impl.php");
    assert_eq!(proxy.len(), 1);
    assert_eq!(proxy[0].target, None);
    assert!(proxy[0].candidates.is_empty());
    no_internal_markers_or_synthetic_definitions(&fixture);
}
