//! Board-only test helpers; disk scratch lives in `crate::test_fixtures`.
use std::io::Read;

/// Reads into `buffer`, retrying interrupted syscalls: signal tests run
/// beside blocking test reads under the full suite, so a raw `read` can
/// return `ErrorKind::Interrupted`, which callers must retry.
pub(crate) fn read_ignoring_interrupts(
    stream: &mut std::net::TcpStream,
    buffer: &mut [u8],
) -> std::io::Result<usize> {
    loop {
        match stream.read(buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}
