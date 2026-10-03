use super::*;

#[test]
fn repository_identity_overrides_use_strict_typed_repo_keys() {
    let defaults = || {
        BoardConfig::for_database(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/overrides.sqlite3"),
        )
    };
    let key = "a".repeat(40);
    let input = format!("[repos]\n'https://example.test/repo' = '{key}'");
    let config = BoardConfig::from_toml(&input, defaults()).unwrap();
    assert_eq!(config.repos["https://example.test/repo"].as_str(), key);
    assert!(BoardConfig::from_toml("[repos]\n'origin' = 'not-an-oid'", defaults()).is_err());
}

#[test]
fn repository_overrides_normalize_detected_origins_and_reject_conflicting_aliases() {
    let defaults = || {
        BoardConfig::for_database(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/normalized-overrides.sqlite3"),
        )
    };
    let first = "a".repeat(40);
    let second = "b".repeat(40);
    let input = format!(
        "[repos]\n'https://user:secret@example.test/repo' = '{first}'\n'https://example.test/repo' = '{first}'"
    );
    let config = BoardConfig::from_toml(&input, defaults()).unwrap();
    assert_eq!(config.repos.len(), 1);
    assert_eq!(config.repos["https://example.test/repo"].as_str(), first);
    let conflict = format!(
        "[repos]\n'https://user:secret@example.test/repo' = '{first}'\n'https://example.test/repo' = '{second}'"
    );
    let error = BoardConfig::from_toml(&conflict, defaults()).unwrap_err();
    assert!(error.to_string().starts_with("invalid_options:"));
    assert!(error.to_string().contains(&first));
    assert!(error.to_string().contains(&second));
}

#[test]
fn relative_board_database_configuration_is_rejected() {
    let error = BoardConfig::for_database("relative/board.sqlite3")
        .validate()
        .unwrap_err();
    assert!(error.to_string().starts_with("invalid_options:"));
}

#[test]
fn strict_config_rejects_unknown_fields_and_invalid_ttl() {
    let defaults = || {
        BoardConfig::for_database(
            crate::board::board_test_support::scratch("board-config-")
                .path()
                .join("board.sqlite3"),
        )
    };
    assert!(BoardConfig::from_toml("usre = 'josh'", defaults()).is_err());
    assert!(BoardConfig::from_toml("claim_ttl_minutes = 0", defaults()).is_err());
    let config = BoardConfig::from_toml(
        "user = 'josh'\nhost = 'laptop'\nclaim_ttl_minutes = 90",
        defaults(),
    )
    .unwrap();
    assert_eq!(config.claim_ttl_seconds(), 5400);
    assert_eq!(
        config.actor(Some("codex"), Some("c1")).unwrap().identity(),
        "josh@laptop/codex/c1"
    );
    assert_eq!(config.actor(None, None).unwrap().harness.as_str(), "cli");
}

#[test]
fn remote_mode_is_preserved_but_explicitly_unsupported() {
    let config = BoardConfig::from_toml(
        "mode = 'remote'\nurl = 'https://board.example'",
        BoardConfig::for_database(
            crate::board::board_test_support::scratch("board-config-")
                .path()
                .join("board.sqlite3"),
        ),
    )
    .unwrap();
    assert_eq!(config.mode, BoardMode::Remote);
    assert!(
        config
            .ensure_local()
            .unwrap_err()
            .to_string()
            .starts_with("board_remote_unsupported:")
    );
}

#[cfg(unix)]
#[test]
fn os_identity_uses_a_passwd_home_and_captured_hostname() {
    let account = passwd_account().unwrap();
    assert!(account.home.is_absolute());
    assert!(!account.user.is_empty());
    assert!(!machine_hostname().unwrap().is_empty());
}
