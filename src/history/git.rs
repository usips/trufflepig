//! Read-only Git plumbing with bounded subprocess lifetime and output.

use crate::identity::GitOid;
use anyhow::{Context, Result, bail, ensure};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const MAX_BLOB_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub struct GitRepository {
    pub root: PathBuf,
    pub common_dir: PathBuf,
    /// Repository-relative root, empty at repository root, otherwise ending in `/`.
    pub root_prefix: String,
}

impl GitRepository {
    pub fn discover(root: &Path) -> Result<Self> {
        let root = root.canonicalize().context("canonicalize history root")?;
        let version = execute(&root, &["version"], None)?;
        let version = std::str::from_utf8(&version)?
            .split_whitespace()
            .nth(2)
            .unwrap_or("");
        let mut parts = version.split('.');
        let major: u32 = parts.next().unwrap_or("").parse()?;
        let minor: u32 = parts.next().unwrap_or("").parse()?;
        ensure!(
            (major, minor) >= (2, 55),
            "history requires Git 2.55 or newer"
        );
        let top = output_path(execute(&root, &["rev-parse", "--show-toplevel"], None)?)?;
        let common_dir = output_path(execute(
            &root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            None,
        )?)?
        .canonicalize()?;
        let prefix = root
            .strip_prefix(&top)
            .context("history root is outside repository")?;
        let mut root_prefix = prefix
            .to_str()
            .context("history root requires UTF-8")?
            .to_owned();
        if !root_prefix.is_empty() {
            root_prefix.push('/');
        }
        let repository = Self {
            root: top,
            common_dir,
            root_prefix,
        };
        ensure!(
            !repository.common_dir.join("info/grafts").exists(),
            "history does not support grafts"
        );
        ensure!(
            repository
                .run(&["for-each-ref", "--format=%(refname)", "refs/replace/"])?
                .is_empty(),
            "history does not support replacement refs"
        );
        Ok(repository)
    }

    pub fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        execute(&self.root, args, None)
    }

    pub fn run_with_input(&self, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            input.len() <= MAX_BLOB_BYTES,
            "Git input exceeds blob resource limit"
        );
        execute(&self.root, args, Some(input))
    }

    pub fn resolve(&self, revision: &str) -> Result<GitOid> {
        ensure!(
            !revision.is_empty() && !revision.starts_with('-'),
            "invalid revision"
        );
        let expression = format!("{revision}^{{commit}}");
        let output = self.run(&["rev-parse", "--verify", "--end-of-options", &expression])?;
        GitOid::parse(std::str::from_utf8(&output)?.trim_end())
    }

    pub fn blob(&self, oid: &GitOid) -> Result<Vec<u8>> {
        read_blob(&self.common_dir, oid.as_str())
    }

    pub fn repository_path(&self, path: &str) -> Result<String> {
        ensure!(
            !path.is_empty()
                && Path::new(path)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
            "target must be a relative path without traversal"
        );
        Ok(format!("{}{path}", self.root_prefix))
    }

    pub fn scoped_path<'a>(&self, path: &'a str) -> Option<&'a str> {
        path.strip_prefix(&self.root_prefix)
    }
}

/// Read an existing blob by exact object identity; never fetch or archive it.
pub fn read_blob(repository: &Path, blob: &str) -> Result<Vec<u8>> {
    let oid = GitOid::parse(blob)?;
    let size = execute(repository, &["cat-file", "-s", oid.as_str()], None)
        .context("historical blob unavailable")?;
    let size: usize = std::str::from_utf8(&size)?.trim_end().parse()?;
    ensure!(
        size <= MAX_BLOB_BYTES,
        "historical blob exceeds 2 MiB resource limit"
    );
    let contents = execute(repository, &["cat-file", "blob", oid.as_str()], None)
        .context("historical blob unavailable")?;
    ensure!(
        contents.len() == size,
        "historical blob size changed during read"
    );
    let actual = execute(
        repository,
        &["hash-object", "--stdin", "--no-filters"],
        Some(&contents),
    )?;
    ensure!(
        std::str::from_utf8(&actual)?.trim_end() == oid.as_str(),
        "historical blob identity verification failed"
    );
    Ok(contents)
}

fn output_path(bytes: Vec<u8>) -> Result<PathBuf> {
    let text = String::from_utf8(bytes)?;
    Ok(PathBuf::from(text.strip_suffix('\n').unwrap_or(&text)))
}

fn execute(directory: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
    let name = args.first().copied().unwrap_or("");
    ensure!(
        matches!(
            name,
            "version"
                | "rev-parse"
                | "rev-list"
                | "for-each-ref"
                | "cat-file"
                | "hash-object"
                | "ls-tree"
                | "diff-tree"
                | "diff"
                | "show"
                | "log"
                | "blame"
                | "ls-files"
                | "status"
                | "check-ignore"
        ),
        "unsupported Git plumbing command"
    );
    ensure!(
        name != "hash-object" || !args.contains(&"-w"),
        "Git object writes are forbidden"
    );
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .args([
            "--no-pager",
            "--no-lazy-fetch",
            "--no-optional-locks",
            "--no-replace-objects",
            "--literal-pathspecs",
        ])
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "credential.helper=",
            "-c",
            "protocol.allow=never",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "submodule.recurse=false",
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .arg(name);
    if matches!(name, "diff" | "diff-tree" | "show" | "log") {
        command.args(["--no-ext-diff", "--no-textconv"]);
    }
    if name == "blame" {
        command.arg("--no-textconv");
    }
    command.args(&args[1..]);
    collect_process(command, input, COMMAND_TIMEOUT, MAX_OUTPUT_BYTES)
}

struct ProcessGroup(Child);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // Every subprocess is its own process group; descendants cannot retain pipes.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> Result<()> {
    let descriptor = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    ensure!(
        flags >= 0
            && unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "configure Git pipe"
    );
    Ok(())
}

fn collect_process(
    mut command: Command,
    input: Option<&[u8]>,
    timeout: Duration,
    limit: usize,
) -> Result<Vec<u8>> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = ProcessGroup(command.spawn().context("start Git plumbing")?);
    let mut stdout = child.0.stdout.take().context("Git stdout unavailable")?;
    let mut stderr = child.0.stderr.take().context("Git stderr unavailable")?;
    let mut stdin = child.0.stdin.take();
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    if let Some(pipe) = &stdin {
        nonblocking(pipe)?;
    }
    let mut remaining = input.unwrap_or_default();
    let mut output = Vec::with_capacity(8192.min(limit));
    let mut errors = Vec::with_capacity(1024.min(limit));
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    let start = Instant::now();
    loop {
        ensure!(start.elapsed() < timeout, "Git plumbing timed out");
        if let Some(pipe) = &mut stdin {
            match pipe.write(remaining) {
                Ok(count) => remaining = &remaining[count..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => remaining = &[],
                Err(error) => return Err(error.into()),
            }
            if remaining.is_empty() {
                stdin = None;
            }
        }
        drain_pipe(
            &mut stdout,
            &mut output,
            &mut stdout_closed,
            limit.saturating_sub(errors.len()),
        )?;
        drain_pipe(
            &mut stderr,
            &mut errors,
            &mut stderr_closed,
            limit.saturating_sub(output.len()),
        )?;
        if let Some(status) = child.0.try_wait()? {
            if stdout_closed && stderr_closed {
                if !status.success() {
                    bail!(
                        "Git plumbing failed: {}",
                        String::from_utf8_lossy(&errors[..errors.len().min(2048)])
                    );
                }
                return Ok(output);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn drain_pipe(
    pipe: &mut impl Read,
    output: &mut Vec<u8>,
    closed: &mut bool,
    limit: usize,
) -> Result<()> {
    let mut buffer = [0; 8192];
    // Bound work per tick so a continuously producing child cannot defeat timeout.
    for _ in 0..32 {
        match pipe.read(&mut buffer) {
            Ok(0) => {
                *closed = true;
                break;
            }
            Ok(count) => {
                ensure!(
                    output.len().saturating_add(count) <= limit,
                    "Git output exceeds resource limit"
                );
                output.reserve(count);
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args([
                "-c",
                "user.name=History Test",
                "-c",
                "user.email=history@example.invalid",
            ])
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .trim_end()
            .to_owned()
    }

    fn repository(format: &str) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        git(
            directory.path(),
            &["init", &format!("--object-format={format}")],
        );
        std::fs::create_dir(directory.path().join("inside")).unwrap();
        std::fs::write(
            directory.path().join("inside/[literal].rs"),
            b"fn original() {}\r\n",
        )
        .unwrap();
        git(directory.path(), &["add", "."]);
        git(directory.path(), &["commit", "-m", "initial"]);
        directory
    }

    #[test]
    fn git_exact_blobs_and_scope_support_both_object_formats() {
        for format in ["sha1", "sha256"] {
            let directory = repository(format);
            let repository = GitRepository::discover(&directory.path().join("inside")).unwrap();
            assert_eq!(repository.root_prefix, "inside/");
            assert_eq!(
                repository.repository_path("[literal].rs").unwrap(),
                "inside/[literal].rs"
            );
            assert!(repository.repository_path("../outside.rs").is_err());
            assert_eq!(repository.scoped_path("outside.rs"), None);
            assert_eq!(
                repository.scoped_path("inside/[literal].rs"),
                Some("[literal].rs")
            );
            let head = repository.resolve("HEAD").unwrap();
            assert_eq!(head.as_str().len(), if format == "sha1" { 40 } else { 64 });
            let oid = git(directory.path(), &["rev-parse", "HEAD:inside/[literal].rs"]);
            std::fs::write(directory.path().join("inside/[literal].rs"), b"edited").unwrap();
            assert_eq!(
                read_blob(&repository.common_dir, &oid).unwrap(),
                b"fn original() {}\r\n"
            );
            assert!(repository.resolve("--all").is_err());
            assert!(read_blob(&repository.common_dir, &"0".repeat(oid.len())).is_err());
        }
    }

    #[test]
    fn git_linked_worktrees_share_common_identity() {
        let directory = repository("sha1");
        let linked = directory.path().join("linked");
        git(
            directory.path(),
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );
        let original = GitRepository::discover(directory.path()).unwrap();
        let other = GitRepository::discover(&linked).unwrap();
        assert_eq!(original.common_dir, other.common_dir);
        assert_ne!(original.root, other.root);
    }

    #[test]
    fn git_literal_paths_and_external_driver_suppression() {
        let directory = repository("sha1");
        std::fs::write(directory.path().join("inside/l.rs"), b"other").unwrap();
        git(directory.path(), &["add", "."]);
        git(directory.path(), &["commit", "-m", "second"]);
        let repository = GitRepository::discover(directory.path()).unwrap();
        let output = repository
            .run(&[
                "ls-tree",
                "--name-only",
                "HEAD",
                "--",
                "inside/[literal].rs",
            ])
            .unwrap();
        assert_eq!(output, b"inside/[literal].rs\n");
        git(directory.path(), &["config", "diff.external", "false"]);
        std::fs::write(directory.path().join("inside/[literal].rs"), b"changed\n").unwrap();
        assert!(
            repository
                .run(&["diff", "HEAD", "--", "inside/[literal].rs"])
                .is_ok()
        );
    }

    #[test]
    fn git_blob_resource_limit_and_replacements_are_explicit() {
        let directory = repository("sha1");
        std::fs::write(directory.path().join("large"), vec![0; MAX_BLOB_BYTES + 1]).unwrap();
        let oid = git(directory.path(), &["hash-object", "-w", "large"]);
        let repository = GitRepository::discover(directory.path()).unwrap();
        assert!(
            read_blob(&repository.common_dir, &oid)
                .unwrap_err()
                .to_string()
                .contains("resource limit")
        );
        assert!(repository.run(&["hash-object", "-w", "large"]).is_err());
        git(
            directory.path(),
            &["update-ref", &format!("refs/replace/{oid}"), &oid],
        );
        assert!(
            GitRepository::discover(directory.path())
                .unwrap_err()
                .to_string()
                .contains("replacement refs")
        );
    }

    #[test]
    fn git_process_bounds_cover_output_and_descendant_lifetime() {
        let mut flood = Command::new("sh");
        flood.args(["-c", "while :; do printf '0123456789abcdef'; done"]);
        assert!(
            collect_process(flood, None, Duration::from_secs(2), 1024)
                .unwrap_err()
                .to_string()
                .contains("resource limit")
        );
        let mut retained_pipe = Command::new("sh");
        retained_pipe.args(["-c", "sleep 10 & exit 0"]);
        let started = Instant::now();
        assert!(
            collect_process(retained_pipe, None, Duration::from_millis(100), 1024)
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
