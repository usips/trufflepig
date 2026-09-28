use super::{AliasSource, AliasSourceIssue, AliasableNamespace};

#[test]
fn reads_ordered_php_legacy_and_short_array_literals() {
    let source = br#"<?php
class XF
{
    public static function getAliasableNamespaces(): array
    {
        return array(
            'ConnectedAccount\\Service' => 'Service',
            'Service',
            'ActivityLog' => 'Handler',
            'Controller',
            'Webhook\\Event' => 'Handler',
        );
    }

    public static function getUnaliasableNamespaces(): array
    {
        return ['Psr', 'Symfony', 'enshrined', 'lbuchs'];
    }
}
"#;
    let parsed = AliasSource::parse(source);

    assert_eq!(
        parsed.aliasable_namespaces,
        [
            AliasableNamespace {
                namespace: "ConnectedAccount\\Service".into(),
                suffix: "Service".into(),
            },
            AliasableNamespace {
                namespace: "Service".into(),
                suffix: "Service".into(),
            },
            AliasableNamespace {
                namespace: "ActivityLog".into(),
                suffix: "Handler".into(),
            },
            AliasableNamespace {
                namespace: "Controller".into(),
                suffix: "Controller".into(),
            },
            AliasableNamespace {
                namespace: "Webhook\\Event".into(),
                suffix: "Handler".into(),
            },
        ]
    );
    assert_eq!(
        parsed.unaliasable_namespaces,
        ["Psr", "Symfony", "enshrined", "lbuchs"]
    );
    assert!(parsed.issues.is_empty());
}

#[test]
fn missing_hooks_leave_identity_metadata_without_an_issue() {
    let parsed = AliasSource::parse(b"<?php class XF { public function unrelated() {} }");

    assert!(parsed.aliasable_namespaces.is_empty());
    assert!(parsed.unaliasable_namespaces.is_empty());
    assert!(parsed.issues.is_empty());
}

#[test]
fn ignores_namespaced_classes_named_xf() {
    let source = br#"<?php
namespace Vendor;
class XF
{
    public static function getAliasableNamespaces(): array { return ['Controller']; }
}
"#;
    let parsed = AliasSource::parse(source);

    assert!(parsed.aliasable_namespaces.is_empty());
    assert!(parsed.issues.is_empty());
}

#[test]
fn dynamic_hook_is_reported_and_does_not_guess_rules() {
    let source = br#"<?php
class XF
{
    public static function getAliasableNamespaces(): array
    {
        return self::buildAliasableNamespaces();
    }
}
"#;
    let parsed = AliasSource::parse(source);

    assert!(parsed.aliasable_namespaces.is_empty());
    assert_eq!(parsed.issues, [AliasSourceIssue::AliasableHookUnsupported]);
    assert_eq!(
        parsed.issues[0].code(),
        "xenforo_get_aliasable_namespaces_unsupported"
    );
}

#[test]
fn malformed_source_and_oversized_source_report_bounded_issues() {
    let malformed = AliasSource::parse(b"<?php class XF { function broken( {");
    assert_eq!(malformed.issues, [AliasSourceIssue::ParseError]);

    let oversized = vec![b' '; super::MAX_ALIAS_SOURCE_BYTES + 1];
    let oversized = AliasSource::parse(&oversized);
    assert_eq!(oversized.issues, [AliasSourceIssue::SourceTooLarge]);
}
