use super::*;
use crate::{daemon::deadline::is_timed_out, store::Store, workspace::config::WorkspaceConfig};
use std::{sync::Arc, thread::JoinHandle};

/// Two indexed members plus a coordinator served on a test thread.
struct ServedWorkspace {
    root: tempfile::TempDir,
    base: tempfile::TempDir,
    config: WorkspaceConfig,
    coordinator: PathBuf,
    thread: Option<JoinHandle<Result<()>>>,
}

impl ServedWorkspace {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let mut text = "[workspace]\nname = 'coordinated'\n".to_owned();
        for member in ["engine", "pack"] {
            std::fs::create_dir(root.path().join(member)).unwrap();
            std::fs::write(
                root.path().join(member).join("lib.rs"),
                format!("pub struct SharedThing {{ pub {member}_marker: u8 }}\n"),
            )
            .unwrap();
            text.push_str(&format!("[members.{member}]\npath = '{member}'\n"));
        }
        let path = root.path().join("trufflepig.workspace.toml");
        std::fs::write(&path, text).unwrap();
        let config = WorkspaceConfig::load(&path).unwrap();
        for member in &config.members {
            let member = MemberRoot::configured(member);
            let cache = member_cache(&member, Some(base.path())).unwrap();
            Store::open(&member.root, &cache).unwrap().index().unwrap();
        }
        let coordinator = cache_path(&config, Some(base.path())).unwrap();
        WorkspaceResults::create(&coordinator).unwrap();
        let handler = WorkspaceCoordinator {
            config_path: config.path.clone(),
            config_id: config.id.clone(),
            cache: coordinator.clone(),
        };
        let (serve_root, serve_cache) = (root.path().to_owned(), coordinator.clone());
        let thread = std::thread::spawn(move || {
            daemon::serve_coordinator(&serve_root, &serve_cache, handler)
        });
        let started = Instant::now();
        while !coordinator.join("daemon.sock").exists() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "coordinator never bound"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        Self {
            root,
            base,
            config,
            coordinator,
            thread: Some(thread),
        }
    }

    fn args(&self, flags: &[&str], words: &[&str]) -> Vec<String> {
        let mut args = vec![
            "--workspace".to_owned(),
            self.config.path.display().to_string(),
            "--root".to_owned(),
            self.root.path().join("engine").display().to_string(),
            "--cache".to_owned(),
            self.base.path().display().to_string(),
            "--diagnostics".to_owned(),
            "off".to_owned(),
        ];
        args.extend(flags.iter().chain(words).map(|word| (*word).to_owned()));
        args
    }

    fn ask(&self, flags: &[&str], words: &[&str]) -> Result<String> {
        let args = self.args(flags, words);
        daemon::request(&self.coordinator, &args, &RequestContext::new(None, None))?
            .context("coordinator is not listening")
    }

    fn engine_cache(&self) -> PathBuf {
        member_cache(
            &MemberRoot::configured(&self.config.members[0]),
            Some(self.base.path()),
        )
        .unwrap()
    }
}

impl Drop for ServedWorkspace {
    fn drop(&mut self) {
        let _ = daemon::stop(&self.coordinator);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn coordinator_answers_parallel_clients() {
    let workspace = Arc::new(ServedWorkspace::start());
    let clients: Vec<_> = (0..10)
        .map(|_| {
            let workspace = Arc::clone(&workspace);
            std::thread::spawn(move || {
                workspace.ask(&["--no-daemon"], &["search", "sym:SharedThing", "ws:all"])
            })
        })
        .collect();
    for client in clients {
        let reply: serde_json::Value =
            serde_json::from_str(&client.join().unwrap().unwrap()).unwrap();
        let members: Vec<_> = reply["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["member"].as_str().unwrap().to_owned())
            .collect();
        assert!(members.contains(&"engine".to_owned()), "{reply}");
        assert!(members.contains(&"pack".to_owned()), "{reply}");
    }
}

#[test]
fn coordinator_deadline_starts_at_accept() {
    let workspace = ServedWorkspace::start();
    let handler = WorkspaceCoordinator {
        config_path: workspace.config.path.clone(),
        config_id: workspace.config.id.clone(),
        cache: workspace.coordinator.clone(),
    };
    let error = handler
        .request(AcceptedRequest {
            context: RequestContext::new(None, None),
            args: workspace.args(&["--no-daemon"], &["search", "sym:SharedThing", "ws:all"]),
            deadline: QueryDeadline::after(Duration::ZERO),
        })
        .unwrap_err();
    assert!(is_timed_out(&error), "{error:#}");
}

/// Owner verbs reach the member's root daemon directly; they never re-enter a
/// router that is still proxying the request (the bare-`status` deadlock).
#[test]
fn coordinator_owner_status_reaches_the_root_daemon_directly() {
    let workspace = ServedWorkspace::start();
    let engine_root = workspace.root.path().join("engine");
    let engine_cache = workspace.engine_cache();
    let root_daemon = {
        let args: Vec<String> = [
            "--no-workspace",
            "--diagnostics",
            "off",
            "--root",
            engine_root.to_str().unwrap(),
            "--cache",
            engine_cache.to_str().unwrap(),
            "serve",
        ]
        .map(str::to_owned)
        .into();
        std::thread::spawn(move || crate::cli::run(&args))
    };
    let started = Instant::now();
    while !engine_cache.join("daemon.sock").exists() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "root daemon never bound"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    // The tokenizer's one-time load is not part of the request path under test.
    crate::output::OutputBudget::new(100)
        .unwrap()
        .render(&serde_json::json!({}))
        .unwrap();
    let started = Instant::now();
    let status = workspace.ask(&[], &["status"]);
    let elapsed = started.elapsed();
    daemon::stop(&engine_cache).unwrap();
    root_daemon.join().unwrap().unwrap();
    let status: serde_json::Value = serde_json::from_str(&status.unwrap()).unwrap();
    // The re-entry deadlock this guards against waits out the 28 s proxy
    // timeout; the bound leaves room for a heavily loaded test machine.
    assert!(elapsed < Duration::from_secs(20), "status took {elapsed:?}");
    assert_eq!(status["member"], "engine");
    assert!(status["generation"].as_i64().unwrap() >= 1, "{status}");
}
