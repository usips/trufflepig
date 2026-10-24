use super::*;
use crate::{daemon::deadline::QueryDeadline, store::Store, workspace::config::WorkspaceConfig};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread::JoinHandle,
};

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
        let deadline = QueryDeadline::start();
        WorkspaceResults::create(&coordinator, &deadline).unwrap();
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

    fn options_with_cache(&self, cache: &Path, words: &[&str]) -> crate::cli::Arguments {
        let mut args = vec![
            "--workspace".to_owned(),
            self.config.path.display().to_string(),
            "--root".to_owned(),
            self.root.path().join("engine").display().to_string(),
            "--cache".to_owned(),
            cache.display().to_string(),
            "--budget".to_owned(),
            "256".to_owned(),
            "--diagnostics".to_owned(),
            "off".to_owned(),
        ];
        args.extend(words.iter().map(|word| (*word).to_owned()));
        crate::cli::parse(&args).unwrap()
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

/// Root daemons own reconciliation while coordinator reads use published member stores.
struct MemberDaemons(Vec<(PathBuf, JoinHandle<Result<()>>)>);

impl MemberDaemons {
    fn start(workspace: &ServedWorkspace) -> Self {
        let mut daemons = Self(Vec::with_capacity(workspace.config.members.len()));
        for member in &workspace.config.members {
            let member_root = MemberRoot::configured(member);
            let cache = member_cache(&member_root, Some(workspace.base.path())).unwrap();
            let args: Vec<String> = [
                "--no-workspace",
                "--diagnostics",
                "off",
                "--root",
                member_root.root.to_str().unwrap(),
                "--cache",
                cache.to_str().unwrap(),
                "serve",
            ]
            .map(str::to_owned)
            .into();
            let thread = std::thread::spawn(move || crate::cli::run(&args).map(|_| ()));
            daemons.0.push((cache.clone(), thread));
            let started = Instant::now();
            while !cache.join("daemon.sock").exists() && !daemons.0.last().unwrap().1.is_finished()
            {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "member daemon for {} never bound",
                    member_root.name()
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(
                cache.join("daemon.sock").exists(),
                "member daemon for {} exited before binding",
                member_root.name()
            );
        }
        daemons
    }
}

impl Drop for MemberDaemons {
    fn drop(&mut self) {
        for (cache, _) in &self.0 {
            let _ = daemon::stop(cache);
        }
        for (_, thread) in self.0.drain(..) {
            let _ = thread.join();
        }
    }
}

#[test]
fn coordinator_answers_parallel_clients() {
    let workspace = Arc::new(ServedWorkspace::start());
    let _member_daemons = MemberDaemons::start(&workspace);
    let clients: Vec<_> = (0..10)
        .map(|_| {
            let workspace = Arc::clone(&workspace);
            std::thread::spawn(move || workspace.ask(&[], &["search", "sym:SharedThing", "ws:all"]))
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
        let coverage = reply["coverage"].as_array().expect("member coverage");
        assert_eq!(coverage.len(), 2, "{reply}");
        assert!(
            coverage.iter().all(|member| member["state"] == "searched"),
            "{reply}"
        );
        assert!(
            coverage.iter().all(|member| member["partial"] == false),
            "{reply}"
        );
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
    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&error),
        "{error:#}"
    );
}

#[test]
fn current_path_show_and_continuation_bypass_a_blocked_workspace_cache() {
    let workspace = ServedWorkspace::start();
    let source_path = workspace.root.path().join("engine/large.rs");
    let body = (0..240)
        .map(|line| {
            format!(
                "pub fn row_{line:03}() {{ let value = \"{}\"; }}\n",
                "payload ".repeat(20)
            )
        })
        .collect::<String>();
    std::fs::write(&source_path, body).unwrap();

    // A regular file cannot contain either the coordinator or member cache directories.
    let blocked_cache = workspace.base.path().join("blocked-cache");
    std::fs::write(&blocked_cache, b"keep this cache path blocked").unwrap();
    let config = WorkspaceConfig::load(&workspace.config.path).unwrap();
    let coordinator_cache = cache_path(&config, Some(&blocked_cache)).unwrap();
    let context = RequestContext::new(None, None);
    let mut options = workspace.options_with_cache(&blocked_cache, &["show", "large.rs"]);
    options.budget = 512;
    options.no_daemon = true;

    let first =
        super::run_with_deadline(config.clone(), &options, &context, QueryDeadline::start())
            .unwrap();
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["path"], "large.rs");
    assert_eq!(first["source"], "current_file");
    assert_eq!(first["verified"], false);
    assert_eq!(first["member"], "engine");
    assert_eq!(first["workspace"], "coordinated");
    assert_eq!(
        first["repository"],
        crate::store::encode_path(&workspace.root.path().join("engine"))
    );
    assert!(first["truncated"].as_bool().unwrap(), "{first}");
    let continuation = first["next"].as_str().expect("stateless continuation");
    assert!(continuation.starts_with("read:path:"), "{continuation}");
    let first_offset = continuation
        .rsplit_once('@')
        .unwrap()
        .1
        .parse::<u64>()
        .unwrap();

    // The normal client path also bypasses coordinator creation and forwarding.
    options.words[1] = continuation.to_owned();
    options.no_daemon = false;
    let second =
        super::run_with_deadline(config.clone(), &options, &context, QueryDeadline::start())
            .unwrap();
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(second["start"].as_u64().unwrap(), first_offset, "{second}");
    assert_eq!(second["member"], "engine");
    let second_continuation = second["next"].as_str().expect("second continuation");
    let second_offset = second_continuation
        .rsplit_once('@')
        .unwrap()
        .1
        .parse::<u64>()
        .unwrap();

    // A forwarded request also reaches local() before WorkspaceResults::open.
    options.words[1] = second_continuation.to_owned();
    let handler = WorkspaceCoordinator {
        config_path: config.path.clone(),
        config_id: config.id.clone(),
        cache: coordinator_cache,
    };
    let third = handler
        .request(AcceptedRequest {
            context: RequestContext::new(None, None),
            args: normalized_args(&options, &options.root),
            deadline: QueryDeadline::start(),
        })
        .unwrap();
    let third: serde_json::Value = serde_json::from_str(&third).unwrap();
    assert_eq!(third["start"].as_u64().unwrap(), second_offset, "{third}");
    assert_eq!(third["member"], "engine");
    assert!(third["next"].as_str().is_some(), "{third}");

    options.words[1] = "large.rs".into();
    let expired = super::run_with_deadline(
        config,
        &options,
        &context,
        QueryDeadline::after(Duration::ZERO),
    )
    .unwrap_err();
    assert!(
        crate::coded_error::ErrorCode::TimedOut.is_code_of(&expired),
        "{expired:#}"
    );
    assert_eq!(
        std::fs::read(&blocked_cache).unwrap(),
        b"keep this cache path blocked"
    );
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
    let started = Instant::now();
    let status = workspace.ask(&[], &["status"]);
    let elapsed = started.elapsed();
    daemon::stop(&engine_cache).unwrap();
    root_daemon.join().unwrap().unwrap();
    let status: serde_json::Value = serde_json::from_str(&status.unwrap()).unwrap();
    assert!(elapsed < Duration::from_secs(5), "status took {elapsed:?}");
    assert_eq!(status["member"], "engine");
    assert!(status["generation"].as_i64().unwrap() >= 1, "{status}");
}
