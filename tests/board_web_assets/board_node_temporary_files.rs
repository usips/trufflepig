use std::{env, io, path::PathBuf};

pub(crate) fn node_temporary_directory() -> io::Result<PathBuf> {
    let directory = env::var_os("TMPDIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".cache/codex-tmp"))
        })
        .ok_or_else(|| {
            io::Error::other("TMPDIR or HOME is required for disk-backed Node output")
        })?;
    disk_directory(directory)
}

fn disk_directory(directory: PathBuf) -> io::Result<PathBuf> {
    let ancestor = directory
        .ancestors()
        .find(|path| path.exists())
        .ok_or_else(|| io::Error::other("Node scratch directory has no existing ancestor"))?;
    if ancestor.canonicalize()?.starts_with("/tmp") {
        return Err(io::Error::other(
            "Node scratch files cannot use RAM-backed /tmp",
        ));
    }
    std::fs::create_dir_all(&directory)?;
    directory.canonicalize()
}

#[test]
fn node_scratch_refuses_ram_backed_tmp() {
    if std::path::Path::new("/tmp").is_dir() {
        assert!(disk_directory(PathBuf::from("/tmp")).is_err());
    }
}
