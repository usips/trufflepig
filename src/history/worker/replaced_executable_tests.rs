//! History start refuses a running executable that was replaced on disk.
use crate::background_process::{BackgroundOwner, ReplacedExecutable};

#[test]
fn history_start_rejects_a_deleted_running_executable() {
    crate::test_fixtures::unlinked_executable::run_unlinked(
        "history::worker::replaced_executable_tests::history_start_child_entry",
    );
}

#[test]
fn history_start_child_entry() {
    let Some(child) = crate::test_fixtures::unlinked_executable::unlinked_child() else {
        return;
    };
    let raw_executable = std::env::current_exe().unwrap();
    let error = super::start(&child.root, &child.cache)
        .expect_err("history start rejects the deleted running executable");
    let replaced = error
        .downcast_ref::<ReplacedExecutable>()
        .expect("history start preserves the typed replaced-executable error");
    assert_eq!(replaced.executable, raw_executable);
    assert_eq!(replaced.cache, child.cache.canonicalize().unwrap());
    assert_eq!(replaced.owner, BackgroundOwner::RootDaemon);
    let message = format!("{error:#}");
    assert!(message.starts_with("router_binary_replaced:"), "{message}");
    assert!(
        message.contains(
            child
                .cache
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_ref()
        ),
        "{message}"
    );
    assert!(message.contains("the root daemon started"), "{message}");
    assert!(
        message.contains("stop the detached root daemon for this cache"),
        "{message}"
    );
    assert!(!message.contains("trufflepig system stop"), "{message}");
    assert!(!message.contains("trufflepig-system.service"), "{message}");
}
