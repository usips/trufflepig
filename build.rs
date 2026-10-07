use std::{
    env, fs,
    path::{Path, PathBuf},
};

include!("build/tool_probe.rs");

#[cfg(test)]
mod tests {
    include!("build/tests.rs");
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_file() {
        files.push(path.to_owned());
        return;
    }
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            collect_files(&entry.path(), files);
        }
    }
}

fn main() {
    println!("cargo::rerun-if-changed=build/tool_probe.rs");
    let git = emit_git_cfgs();
    let node = emit_node_cfgs();
    if let Some(out_dir) = env::var_os("OUT_DIR") {
        let report = tool_probe::format_probe_report(&[("git", &git), ("node", &node)]);
        let _ = tool_probe::write_probe_report(Path::new(&out_dir), &report);
    }
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut files = vec![manifest.join("Cargo.toml"), manifest.join("Cargo.lock")];
    collect_files(&manifest.join("src"), &mut files);
    files.sort_by(|left, right| left.cmp(right));
    files.dedup();

    let feature = ["semantic", "semantic-cuda"]
        .into_iter()
        .map(|name| {
            let variable = format!("CARGO_FEATURE_{}", name.replace('-', "_").to_uppercase());
            format!("{name}={}", env::var_os(variable).is_some())
        })
        .collect::<Vec<_>>()
        .join(",");
    let mut lanes = [
        0xcbf29ce484222325_u64,
        0x84222325cbf29ce4_u64,
        0x9e3779b185ebca87_u64,
        0x517cc1b727220a95_u64,
    ];
    hash_bytes(&mut lanes, env!("CARGO_PKG_VERSION").as_bytes());
    hash_bytes(&mut lanes, feature.as_bytes());
    for path in files {
        let relative = path.strip_prefix(&manifest).unwrap_or(&path);
        let label = relative.to_string_lossy();
        println!("cargo:rerun-if-changed={}", path.display());
        hash_bytes(&mut lanes, label.as_bytes());
        if let Ok(contents) = fs::read(&path) {
            hash_bytes(&mut lanes, &contents);
        }
    }
    let build_id = lanes
        .iter()
        .map(|lane| format!("{lane:016x}"))
        .collect::<String>();
    println!("cargo:rustc-env=TRUFFLEPIG_BUILD_ID={build_id}");
}

/// Watch absolute, existing PATH dirs before the selected tool's directory.
/// Missing tools watch every eligible dir; relative or missing dirs never emit.
fn watch_path_dirs(dirs: &[PathBuf], resolved_tool: Option<&Path>) {
    for dir in dirs {
        if resolved_tool.is_some_and(|tool| tool.parent() == Some(dir.as_path())) {
            break;
        }
        if dir.is_absolute() && dir.is_dir() {
            println!("cargo::rerun-if-changed={}", dir.display());
        }
    }
}

/// Expose the installed Git as `board_git_2_46` / `board_git_2_55` cfgs.
/// Missing or unparseable Git emits neither cfg, so gated tests ignore.
fn emit_git_cfgs() -> String {
    println!("cargo::rustc-check-cfg=cfg(board_git_2_46)");
    println!("cargo::rustc-check-cfg=cfg(board_git_2_55)");
    println!("cargo::rerun-if-env-changed=PATH");
    let dirs = tool_probe::path_search_dirs(env::var_os("PATH").as_deref());
    let Some(path) = tool_probe::resolve_tool_in_dirs(&dirs, "git") else {
        watch_path_dirs(&dirs, None);
        return format!("missing (searched {} PATH dirs)", dirs.len());
    };
    watch_path_dirs(&dirs, Some(&path));
    println!("cargo::rerun-if-changed={}", path.display());
    let version = command_stdout(&path, "--version")
        .and_then(|stdout| tool_probe::parse_git_version(&stdout));
    let Some((major, minor)) = version else {
        return format!("unparseable `{}` --version output", path.display());
    };
    if (major, minor) >= (2, 46) {
        println!("cargo::rustc-cfg=board_git_2_46");
    }
    if (major, minor) >= (2, 55) {
        println!("cargo::rustc-cfg=board_git_2_55");
    }
    format!("found {} ({major}.{minor})", path.display())
}

/// Expose a Node >=20 runtime as the `board_node_20` cfg for asset tests.
/// Missing or unparseable Node emits no cfg, so the runner test ignores.
fn emit_node_cfgs() -> String {
    println!("cargo::rustc-check-cfg=cfg(board_node_20)");
    println!("cargo::rerun-if-env-changed=PATH");
    let dirs = tool_probe::path_search_dirs(env::var_os("PATH").as_deref());
    let Some(path) = tool_probe::resolve_tool_in_dirs(&dirs, "node") else {
        watch_path_dirs(&dirs, None);
        return format!("missing (searched {} PATH dirs)", dirs.len());
    };
    watch_path_dirs(&dirs, Some(&path));
    println!("cargo::rerun-if-changed={}", path.display());
    let version = command_stdout(&path, "--version")
        .and_then(|stdout| tool_probe::parse_node_version(&stdout));
    let Some(major) = version else {
        return format!("unparseable `{}` --version output", path.display());
    };
    if major >= 20 {
        println!("cargo::rustc-cfg=board_node_20");
    }
    format!("found {} (v{major})", path.display())
}

/// Run `path` with one argument, returning stdout on success.
fn command_stdout(path: &Path, arg: &str) -> Option<Vec<u8>> {
    std::process::Command::new(path)
        .arg(arg)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
}

fn hash_bytes(lanes: &mut [u64; 4], bytes: &[u8]) {
    for &byte in bytes {
        for lane in lanes.iter_mut() {
            *lane ^= u64::from(byte);
            *lane = lane.wrapping_mul(0x100000001b3);
        }
    }
}
