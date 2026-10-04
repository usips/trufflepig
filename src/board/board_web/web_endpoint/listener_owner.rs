//! Listener ownership via /proc/net/tcp and /proc/net/tcp6 LISTEN rows.
//! A squatter binding the published port while board-serve is down must never
//! pass as the real service, so its socket uid must equal our euid.

use anyhow::{Context, Result, bail};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
};

const PROC_NET_TCP: &str = "/proc/net/tcp";
const PROC_NET_TCP6: &str = "/proc/net/tcp6";
const LISTEN_STATE: u8 = 0x0A;

pub(super) fn require_owned_listener(address: &SocketAddr) -> Result<()> {
    let table = match address {
        SocketAddr::V4(_) => Path::new(PROC_NET_TCP),
        SocketAddr::V6(_) => Path::new(PROC_NET_TCP6),
    };
    require_owned_listener_at(address, table)
}

fn require_owned_listener_at(address: &SocketAddr, table: &Path) -> Result<()> {
    let content = std::fs::read_to_string(table)
        .with_context(|| format!("board_unavailable: cannot inspect {}", table.display()))?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    match listener_uid(&content, address) {
        Some(uid) if uid == euid => Ok(()),
        Some(uid) => bail!(
            "board_unavailable: web listener at {address} is owned by uid {uid}, not our uid {euid}"
        ),
        None => bail!("board_unavailable: no listener at {address}"),
    }
}

fn listener_uid(table: &str, address: &SocketAddr) -> Option<u32> {
    table
        .lines()
        .skip(1)
        .filter_map(parse_row)
        .find(|row| row.state == LISTEN_STATE && row.local == *address)
        .map(|row| row.uid)
}

struct ListenRow {
    local: SocketAddr,
    state: u8,
    uid: u32,
}

fn parse_row(line: &str) -> Option<ListenRow> {
    let mut fields = line.split_whitespace();
    fields.next()?; // sl
    let local = parse_proc_address(fields.next()?)?;
    fields.next()?; // rem_address
    let state = u8::from_str_radix(fields.next()?, 16).ok()?;
    fields.next()?; // tx_queue:rx_queue
    fields.next()?; // tr:tm->when
    fields.next()?; // retrnsmt
    let uid = fields.next()?.parse().ok()?;
    Some(ListenRow { local, state, uid })
}

fn parse_proc_address(text: &str) -> Option<SocketAddr> {
    let (host, port) = text.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match host.len() {
        8 => {
            let raw = u32::from_str_radix(host, 16).ok()?;
            IpAddr::V4(Ipv4Addr::from(raw.swap_bytes()))
        }
        32 => {
            let mut words = [0; 4];
            for (index, word) in words.iter_mut().enumerate() {
                let raw = u32::from_str_radix(host.get(index * 8..index * 8 + 8)?, 16).ok()?;
                *word = raw.swap_bytes();
            }
            let value = u128::from(words[0]) << 96
                | u128::from(words[1]) << 64
                | u128::from(words[2]) << 32
                | u128::from(words[3]);
            IpAddr::V6(Ipv6Addr::from(value))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

    fn row(local: &str, state: &str, uid: u32) -> String {
        format!("   0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000 {uid}        0 12345 1 0000000000000000 100 0 0 10 0")
    }

    fn write_table(directory: &tempfile::TempDir, rows: &[String]) -> std::path::PathBuf {
        let path = directory.path().join("tcp");
        std::fs::write(&path, format!("{HEADER}\n{}\n", rows.join("\n"))).unwrap();
        path
    }

    #[test]
    fn owned_listen_row_is_accepted_and_foreign_uid_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let address: SocketAddr = "127.0.0.1:7341".parse().unwrap();
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        let table = write_table(&directory, &[row("0100007F:1CAD", "0A", euid)]);
        require_owned_listener_at(&address, &table).unwrap();
        let foreign = write_table(&directory, &[row("0100007F:1CAD", "0A", euid + 1)]);
        let error = require_owned_listener_at(&address, &foreign).unwrap_err();
        assert!(
            error.to_string().contains("owned by uid"),
            "{error}"
        );
    }

    #[test]
    fn non_listen_states_and_other_addresses_do_not_count() {
        let directory = tempfile::tempdir().unwrap();
        let address: SocketAddr = "127.0.0.1:7341".parse().unwrap();
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        let rows = [
            row("0100007F:1CAD", "06", euid),  // same address, TIME_WAIT
            row("0100007F:1CAE", "0A", euid),  // different port, LISTEN
            row("0200007F:1CAD", "0A", euid),  // 127.0.0.2, LISTEN
            row("00000000:1CAD", "0A", euid),  // 0.0.0.0 wildcard, LISTEN
        ];
        let table = write_table(&directory, &rows);
        let error = require_owned_listener_at(&address, &table).unwrap_err();
        assert!(error.to_string().contains("no listener"), "{error}");
    }

    #[test]
    fn ipv6_loopback_rows_parse_from_tcp6_tables() {
        let directory = tempfile::tempdir().unwrap();
        let address: SocketAddr = "[::1]:7341".parse().unwrap();
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        let table = write_table(
            &directory,
            &[row("00000000000000000000000001000000:1CAD", "0A", euid)],
        );
        require_owned_listener_at(&address, &table).unwrap();
    }

    #[test]
    fn live_listener_is_recognized_and_departed_listener_is_not() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        require_owned_listener(&address).unwrap();
        drop(listener);
        let error = require_owned_listener(&address).unwrap_err();
        assert!(error.to_string().contains("no listener"), "{error}");
    }
}
