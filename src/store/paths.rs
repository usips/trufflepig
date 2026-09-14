use anyhow::{Result, bail};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Percent encoding preserves every Unix filename byte, including literal percent signs.
pub fn encode_path(path: &Path) -> String {
    let bytes = path.as_os_str().as_bytes();
    let mut encoded = String::with_capacity(bytes.len());
    for &byte in bytes {
        if (0x21..=0x7e).contains(&byte) && !matches!(byte, b'%' | b':' | b'@') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

pub fn decode_path(encoded: &str) -> Result<PathBuf> {
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut cursor = 0;
    let input = encoded.as_bytes();
    while cursor < input.len() {
        if input[cursor] == b'%' {
            if cursor + 2 >= input.len() {
                bail!("invalid percent-encoded path");
            }
            let digits = std::str::from_utf8(&input[cursor + 1..cursor + 3])?;
            bytes.push(u8::from_str_radix(digits, 16)?);
            cursor += 3;
        } else {
            bytes.push(input[cursor]);
            cursor += 1;
        }
    }
    anyhow::ensure!(!bytes.contains(&0), "NUL in path");
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}
