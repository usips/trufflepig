mod local_access_tests;
mod no_daemon_tests;
mod router_api_version_tests;
mod router_probe_cache_tests;
mod router_probe_tests;
mod schema_advice_tests;

use super::*;
use std::{collections::VecDeque, time::Duration};

#[derive(Default)]
struct FakeGateway {
    replies: VecDeque<Result<Option<String>>>,
    requests: Vec<Vec<String>>,
    ensured: usize,
    socket_denied: bool,
}
impl BoardGateway for FakeGateway {
    fn request(&mut self, args: &[String], _: &RequestContext) -> Result<Option<String>> {
        self.requests.push(args.to_vec());
        self.replies.pop_front().unwrap_or(Ok(None))
    }
    fn ensure(&mut self) -> Result<()> {
        self.ensured += 1;
        Ok(())
    }
    fn probe_socket(&mut self, _: &Path) -> std::io::Result<()> {
        if self.socket_denied {
            Err(std::io::ErrorKind::PermissionDenied.into())
        } else {
            Err(std::io::ErrorKind::ConnectionRefused.into())
        }
    }
}

fn scratch() -> tempfile::TempDir {
    crate::board::board_test_support::scratch("board-client-")
}

fn invoke(
    words: &[&str],
    gateway: &mut FakeGateway,
    transport: &mut BoardClientTransport,
    database: &Path,
    runtime: Option<&Path>,
) -> Result<String> {
    let args: Vec<_> = words.iter().map(|word| (*word).to_owned()).collect();
    let options = crate::cli::parse(&args)?;
    let args = crate::board::prepare_client(&args, &options)?;
    let options = crate::cli::parse(&args)?;
    let command = board_grammar::parse(&options, None)?;
    transport.run_prepared(
        &args,
        &options,
        &command,
        &RequestContext::new(None, None),
        gateway,
        &mut || Ok(BoardConfig::for_database(database)),
        runtime,
        &database.parent().unwrap().join("spool"),
    )
}
