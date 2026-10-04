use std::{
    env, fs,
    path::{Path, PathBuf},
};

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
    emit_git_cfgs();
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

/// Expose the installed Git as `board_git_2_46` / `board_git_2_55` cfgs.
/// Missing or unparseable Git emits neither cfg, so gated tests ignore.
fn emit_git_cfgs() {
    println!("cargo::rustc-check-cfg=cfg(board_git_2_46)");
    println!("cargo::rustc-check-cfg=cfg(board_git_2_55)");
    println!("cargo::rerun-if-env-changed=PATH");
    let Some(path) = resolve_on_path("git") else {
        println!("cargo::warning=board Git-gated tests will be ignored: git not found on PATH");
        return;
    };
    println!("cargo::rerun-if-changed={}", path.display());
    let version = std::process::Command::new(&path)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| parse_git_version(&output.stdout));
    let Some((major, minor)) = version else {
        println!("cargo::warning=board Git-gated tests will be ignored: unparseable git --version output");
        return;
    };
    if (major, minor) >= (2, 46) {
        println!("cargo::rustc-cfg=board_git_2_46");
    }
    if (major, minor) >= (2, 55) {
        println!("cargo::rustc-cfg=board_git_2_55");
    }
}

/// Resolve `name` to an executable file via `PATH` search.
fn resolve_on_path(name: &str) -> Option<PathBuf> {
    for dir in env::split_paths(&env::var_os("PATH")?) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let executable = dir.join(format!("{name}.exe"));
            if executable.is_file() {
                return Some(executable);
            }
        }
    }
    None
}

fn parse_git_version(bytes: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut parts = text.split_whitespace().nth(2)?.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn hash_bytes(lanes: &mut [u64; 4], bytes: &[u8]) {
    for &byte in bytes {
        for lane in lanes.iter_mut() {
            *lane ^= u64::from(byte);
            *lane = lane.wrapping_mul(0x100000001b3);
        }
    }
}
