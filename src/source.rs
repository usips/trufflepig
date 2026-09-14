//! Reads are confined to the root and use one buffer for revision verification and output.
use crate::{output::OutputBudget, results, store::Store};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    ffi::CString,
    fs::File,
    io::Read,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::ffi::OsStrExt,
    path::{Component, Path},
};

pub const MAX_READ_BYTES: usize = 16 * 1024 * 1024;

pub fn read_contained(root: &Path, relative: &Path, limit: usize) -> Result<Vec<u8>> {
    let parts = relative.components().collect::<Vec<_>>();
    if parts.is_empty() || parts.iter().any(|p| !matches!(p, Component::Normal(_))) {
        bail!("invalid_path: use a root-relative path without parent components");
    }
    let mut file = File::open(root).context("source_unavailable: cannot open repository root")?;
    for (i, part) in parts.iter().enumerate() {
        let name = CString::new(part.as_os_str().as_bytes()).context("invalid_path: NUL byte")?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if i + 1 < parts.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // Each open is relative to the held directory descriptor; symlinks are never followed.
        let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("source_unavailable: path missing or unsafe");
        }
        // openat returned a new owned descriptor.
        file = unsafe { File::from_raw_fd(fd) };
    }
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("source_unavailable: source is not a regular file");
    }
    if metadata.len() > limit as u64 {
        bail!("source_excluded: file exceeds read limit");
    }
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(limit));
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("source_excluded: file grew beyond read limit");
    }
    Ok(bytes)
}

pub fn line_at(bytes: &[u8], offset: usize) -> usize {
    1 + bytes[..offset.min(bytes.len())]
        .iter()
        .filter(|&&c| c == b'\n')
        .count()
}

pub fn line_span(bytes: &[u8], start: usize, end: usize) -> (usize, usize) {
    (
        line_at(bytes, start),
        line_at(bytes, if end > start { end - 1 } else { start }),
    )
}

fn current_span(bytes: &[u8], first: usize, last: usize) -> Result<(usize, usize)> {
    if first == 0 || last < first {
        bail!("invalid_range: lines must be one-based and inclusive");
    }
    let mut line = 1;
    let mut start = if first == 1 { Some(0) } else { None };
    let mut end = bytes.len();
    for (i, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' {
            if line == last {
                end = i + 1;
                break;
            }
            line += 1;
            if line == first {
                start = Some(i + 1);
            }
        }
    }
    Ok((
        start.context("invalid_range: first line is beyond end of file")?,
        end,
    ))
}

pub fn show(store: &Store, target: &str, budget: &OutputBudget) -> Result<String> {
    let is_handle = target
        .rsplit_once(':')
        .is_some_and(|(id, _)| id.len() == 32 && uuid::Uuid::parse_str(id).is_ok());
    let (path, expected, span, lines) = if is_handle {
        let (_, hit) = results::handle(store, target)?;
        (
            hit.path,
            Some(hit.revision.context(
                "source_excluded: no indexed source revision; use an explicit current path read",
            )?),
            if hit.kind == "file" {
                None
            } else {
                Some((hit.start, hit.end))
            },
            None,
        )
    } else {
        let target = target.strip_prefix("path:").unwrap_or(target);
        if let Some((path, range)) = target.rsplit_once(':') {
            if let Some((a, b)) = range.split_once('-') {
                (
                    path.to_owned(),
                    None,
                    None,
                    Some((a.parse::<usize>()?, b.parse::<usize>()?)),
                )
            } else {
                (target.to_owned(), None, None, None)
            }
        } else {
            (target.to_owned(), None, None, None)
        }
    };
    let decoded = crate::store::decode_path(&path)?;
    let bytes = read_contained(&store.root, &decoded, MAX_READ_BYTES)?;
    let revision = blake3::hash(&bytes).to_hex().to_string();
    if expected.as_ref().is_some_and(|hash| hash != &revision) {
        bail!(
            "stale_source: source revision changed; search again or use explicit current path coordinates"
        );
    }
    let (start, end) = match span {
        Some(span) => span,
        None => current_span(
            &bytes,
            lines.unwrap_or((1, usize::MAX)).0,
            lines.unwrap_or((1, usize::MAX)).1,
        )?,
    };
    if start > end || end > bytes.len() {
        bail!("invalid_span: indexed coordinates outside verified source");
    }
    let (first, last) = line_span(&bytes, start, end);
    let mut offset = start;
    let mut rows = Vec::with_capacity((last - first + 1).min(200));
    for (index, line) in bytes[start..end]
        .split_inclusive(|&b| b == b'\n')
        .take(200)
        .enumerate()
    {
        let (text, encoding) = display_bytes(line);
        rows.push(json!({"line":first+index,"start":offset,"end":offset+line.len(),"text":text,"encoding":encoding}));
        offset += line.len();
    }
    loop {
        let shown_end = rows
            .last()
            .and_then(|r| r["end"].as_u64())
            .unwrap_or(start as u64) as usize;
        let truncated = shown_end < end;
        let next = truncated.then(|| format!("path:{path}:{}-{last}", line_at(&bytes, shown_end)));
        let value = json!({"path":path,"revision":revision,"verified":expected.is_some(),"start":start,"end":end,"lines":rows,"truncated":truncated,"next":next,"tokenizer":"o200k_base"});
        let text = budget.encode(&value)?;
        if budget.fits(&text) && (!rows.is_empty() || start == end) {
            return Ok(text);
        }
        if rows.pop().is_none() {
            bail!("budget_too_small: source line and response envelope do not fit");
        }
    }
}

fn display_bytes(bytes: &[u8]) -> (String, &str) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), "utf8"),
        Err(_) => {
            let mut text = String::with_capacity(bytes.len() * 4);
            for &byte in bytes {
                if (0x20..=0x7e).contains(&byte) && byte != b'\\' {
                    text.push(byte as char);
                } else {
                    use std::fmt::Write;
                    write!(text, "\\x{byte:02x}").expect("string write");
                }
            }
            (text, "byte-escaped")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_traversal_and_symlink_components() {
        let temp = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc", temp.path().join("outside")).unwrap();
        assert!(read_contained(temp.path(), Path::new("outside/passwd"), 4096).is_err());
        assert!(read_contained(temp.path(), Path::new("../x"), 4096).is_err());
    }
    #[test]
    fn original_bom_crlf_and_invalid_bytes() {
        let bytes = b"\xef\xbb\xbffirst\r\n\xffsecond\r\n";
        assert_eq!(current_span(bytes, 2, 2).unwrap(), (10, 19));
        assert_eq!(line_span(bytes, 10, 19), (2, 2));
        assert_eq!(display_bytes(&bytes[10..]).1, "byte-escaped");
    }
}
