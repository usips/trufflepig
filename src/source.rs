//! Reads are confined to the root and use one buffer for revision verification and output.
use crate::{output::OutputBudget, store::Store};
use anyhow::{Context, Result, bail, ensure};
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

pub(crate) mod acquisition;
mod lines;
pub(crate) use acquisition::AcquiredSource;
pub use acquisition::SourceSide;

pub fn show(store: &Store, target: &str, budget: &OutputBudget) -> Result<String> {
    show_with_side(store, target, None, budget)
}

pub fn show_with_side(
    store: &Store,
    target: &str,
    side: Option<SourceSide>,
    budget: &OutputBudget,
) -> Result<String> {
    render(acquisition::acquire(store, target, side)?, budget)
}

fn render(source: AcquiredSource, budget: &OutputBudget) -> Result<String> {
    render_owned(source, budget, &json!({}))
}

pub(crate) fn render_owned(
    source: AcquiredSource,
    budget: &OutputBudget,
    metadata: &serde_json::Value,
) -> Result<String> {
    let metadata = metadata
        .as_object()
        .context("invalid_metadata: source metadata must be an object")?;
    let AcquiredSource {
        bytes,
        path,
        revision,
        span,
        verified,
        handle,
        side,
        historical,
        definitions,
    } = source;
    let (start, end) = (span.start, span.end);
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
        let suffix = side
            .map(|side| format!(":{}", side.as_str()))
            .unwrap_or_default();
        let next = truncated.then(|| format!("read:{handle}{suffix}@{shown_end}"));
        let mut value = json!({"path":path,"revision":revision,"verified":verified,"start":start,"end":end,"lines":rows,"truncated":truncated,"next":next,"tokenizer":"o200k_base"});
        if let Some(identity) = &historical {
            value["historical"] = serde_json::to_value(identity)?;
        }
        if !verified && historical.is_none() {
            // An explicit path read returns the current file, not an indexed revision.
            value["source"] = "current_file".into();
        }
        if let Some((total, also)) = &definitions {
            value["definitions"] = (*total).into();
            value["also"] = also.clone().into();
        }
        let object = value.as_object_mut().expect("source response object");
        ensure!(
            metadata.keys().all(|key| !object.contains_key(key)),
            "invalid_metadata: source metadata collides with response identity"
        );
        object.extend(metadata.clone());
        let text = match budget.format {
            crate::output::OutputFormat::Json => budget.encode(&value)?,
            crate::output::OutputFormat::Lines => lines::show_text(&value),
        };
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
mod tests;
