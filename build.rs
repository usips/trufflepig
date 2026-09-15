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

fn hash_bytes(lanes: &mut [u64; 4], bytes: &[u8]) {
    for &byte in bytes {
        for lane in lanes.iter_mut() {
            *lane ^= u64::from(byte);
            *lane = lane.wrapping_mul(0x100000001b3);
        }
    }
}
