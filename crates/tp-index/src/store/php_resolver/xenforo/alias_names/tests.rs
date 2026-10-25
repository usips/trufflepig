use super::canonicalize_class_name;
use crate::store::php_resolver::xenforo::alias_source::AliasSource;

#[test]
fn canonicalizes_legacy_controller_aliases_and_preserves_complete_names() {
    let source = AliasSource::parse(
        br#"<?php class XF {
            public static function getAliasableNamespaces(): array
            {
                return ['Controller'];
            }
            public static function getUnaliasableNamespaces(): array { return []; }
        }"#,
    );

    assert_eq!(
        canonicalize_class_name("XF\\Pub\\Controller\\Forum", &source),
        Some("XF\\Pub\\Controller\\ForumController".into())
    );
    assert_eq!(
        canonicalize_class_name("XF\\Pub\\Controller\\ForumController", &source),
        Some("XF\\Pub\\Controller\\ForumController".into())
    );
    assert_eq!(
        canonicalize_class_name("\\XF\\Pub\\Controller\\Forum", &source),
        Some("XF\\Pub\\Controller\\ForumController".into())
    );
}

#[test]
fn follows_first_matching_namespace_and_suffix_rules_case_insensitively() {
    let source = AliasSource::parse(
        br#"<?php class XF {
            public static function getAliasableNamespaces(): array
            {
                return ['Pub\\Controller' => 'Admin', 'Controller'];
            }
        }"#,
    );

    assert_eq!(
        canonicalize_class_name("xf\\pub\\controller\\forum", &source),
        Some("xf\\pub\\controller\\forumAdmin".into())
    );
    assert_eq!(
        canonicalize_class_name("XF\\Pub\\Controller\\Forumcontroller", &source),
        Some("XF\\Pub\\Controller\\ForumcontrollerAdmin".into())
    );
}

#[test]
fn vendor_exclusions_are_case_insensitive_and_require_a_namespace_boundary() {
    let source = AliasSource::parse(
        br#"<?php class XF {
            public static function getAliasableNamespaces(): array
            {
                return ['Controller'];
            }
            public static function getUnaliasableNamespaces(): array
            {
                return ['Psr', 'enshrined', 'lbuchs'];
            }
        }"#,
    );

    assert_eq!(
        canonicalize_class_name("PSR\\Log\\LoggerInterface", &source),
        Some("PSR\\Log\\LoggerInterface".into())
    );
    assert_eq!(
        canonicalize_class_name("EnShrined\\SvgSanitize\\Sanitizer", &source),
        Some("EnShrined\\SvgSanitize\\Sanitizer".into())
    );
    assert_eq!(
        canonicalize_class_name("PsrExtra\\Pub\\Controller\\Forum", &source),
        Some("PsrExtra\\Pub\\Controller\\ForumController".into())
    );
}

#[test]
fn absent_alias_hooks_and_unmatched_names_preserve_identity() {
    let source = AliasSource::parse(b"<?php class XF {} ");

    assert_eq!(
        canonicalize_class_name("ShortName", &source),
        Some("ShortName".into())
    );
    assert_eq!(
        canonicalize_class_name("Other\\Thing", &source),
        Some("Other\\Thing".into())
    );
}

#[test]
fn unsupported_alias_hook_blocks_canonical_lookup() {
    let source = AliasSource::parse(
        br#"<?php class XF {
            public static function getAliasableNamespaces(): array
            {
                return self::dynamicAliases();
            }
        }"#,
    );

    assert!(canonicalize_class_name("XF\\Pub\\Controller\\Forum", &source).is_none());
    assert!(canonicalize_class_name("Vendor\\Entity\\User", &source).is_none());
    assert_eq!(
        canonicalize_class_name("ShortName", &source),
        Some("ShortName".into())
    );
}

#[test]
fn unsupported_vendor_hook_blocks_a_known_mapped_namespace() {
    let source = AliasSource::parse(
        br#"<?php class XF {
            public static function getAliasableNamespaces(): array
            {
                return ['Controller'];
            }
            public static function getUnaliasableNamespaces(): array
            {
                return self::dynamicVendorNamespaces();
            }
        }"#,
    );

    assert!(canonicalize_class_name("Vendor\\Controller\\Forum", &source).is_none());
    assert_eq!(
        canonicalize_class_name("Other\\Domain\\Thing", &source),
        Some("Other\\Domain\\Thing".into())
    );
    assert_eq!(
        canonicalize_class_name("Vendor\\Controller\\ForumController", &source),
        Some("Vendor\\Controller\\ForumController".into())
    );
}

#[test]
fn complete_vendor_exclusion_proves_identity_with_dynamic_alias_mappings() {
    let source = AliasSource::parse(
        br#"<?php class XF {
            public static function getAliasableNamespaces(): array
            {
                return self::dynamicAliases();
            }
            public static function getUnaliasableNamespaces(): array
            {
                return ['Psr'];
            }
        }"#,
    );

    assert_eq!(
        canonicalize_class_name("PSR\\Log\\LoggerInterface", &source),
        Some("PSR\\Log\\LoggerInterface".into())
    );
    assert!(canonicalize_class_name("Vendor\\Domain\\Thing", &source).is_none());
}
