/// PATH tool probing used by the build script.
pub mod tool_probe {
    use std::{
        env,
        ffi::OsStr,
        fs, io,
        path::{Path, PathBuf},
    };

    /// Report filename for tool-probe outcomes under `OUT_DIR`.
    pub const REPORT_FILENAME: &str = "board_tool_probes.txt";

    /// Split a `PATH`-style value into search directories, skipping empties.
    pub fn path_search_dirs(path_var: Option<&OsStr>) -> Vec<PathBuf> {
        path_var
            .map(|paths| {
                env::split_paths(paths)
                    .filter(|dir| !dir.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// True when `path` is a regular file with an execute bit set.
    /// Non-Unix targets accept any regular file (no exec-bit model).
    pub fn is_executable_file(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            path.metadata()
                .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            true
        }
    }

    /// Resolve `name` against `dirs`; `is_executable` is injected for tests.
    pub fn resolve_tool_with(
        dirs: &[PathBuf],
        name: &str,
        is_executable: impl Fn(&Path) -> bool,
    ) -> Option<PathBuf> {
        dirs.iter().find_map(|dir| {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
            #[cfg(windows)]
            {
                let executable = dir.join(format!("{name}.exe"));
                if is_executable(&executable) {
                    return Some(executable);
                }
            }
            None
        })
    }

    /// Resolve `name` against `dirs` using the host executable check.
    pub fn resolve_tool_in_dirs(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
        resolve_tool_with(dirs, name, is_executable_file)
    }

    /// Parse `git --version` stdout into `(major, minor)`.
    pub fn parse_git_version(bytes: &[u8]) -> Option<(u32, u32)> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut parts = text.split_whitespace().nth(2)?.split('.');
        Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
    }

    /// Parse `node --version` stdout into its major version.
    pub fn parse_node_version(bytes: &[u8]) -> Option<u32> {
        let text = std::str::from_utf8(bytes).ok()?;
        text.trim()
            .strip_prefix('v')?
            .split('.')
            .next()?
            .parse()
            .ok()
    }

    /// Render the `tool=outcome` probe report, one tool per line.
    pub fn format_probe_report(outcomes: &[(&str, &str)]) -> String {
        let mut report = String::new();
        for (tool, outcome) in outcomes {
            report.push_str(tool);
            report.push('=');
            report.push_str(outcome);
            report.push('\n');
        }
        report
    }

    /// Write `report` to `REPORT_FILENAME` under `out_dir`; return the file.
    pub fn write_probe_report(out_dir: &Path, report: &str) -> io::Result<PathBuf> {
        let path = out_dir.join(REPORT_FILENAME);
        fs::write(&path, report)?;
        Ok(path)
    }
}
