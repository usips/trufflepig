use std::{
    ffi::OsString,
    io,
    path::Path,
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread::{self, ScopedJoinHandle},
    time::{Duration, Instant},
};

mod board_node_output_capture;
mod board_node_pipe_reader;

use board_node_output_capture::{NodeOutputCapture, bounded_diagnostics};
use board_node_pipe_reader::read_pipe;

fn join_pipe_reader(
    reader: ScopedJoinHandle<'_, io::Result<NodeOutputCapture>>,
    invocation: &str,
    stream: &str,
) -> Result<NodeOutputCapture, String> {
    reader
        .join()
        .map_err(|_| format!("{invocation} {stream} reader panicked"))?
        .map_err(|error| format!("read {invocation} {stream}: {error}"))
}

fn join_pipe_readers(
    stdout: ScopedJoinHandle<'_, io::Result<NodeOutputCapture>>,
    stderr: ScopedJoinHandle<'_, io::Result<NodeOutputCapture>>,
    invocation: &str,
    failed: bool,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut stdout = join_pipe_reader(stdout, invocation, "stdout")?;
    let mut stderr = join_pipe_reader(stderr, invocation, "stderr")?;
    if failed {
        stdout
            .print_diagnostics("stdout")
            .and_then(|()| stderr.print_diagnostics("stderr"))
            .map_err(|error| format!("print {invocation} diagnostics: {error}"))?;
    }
    stdout
        .output_bytes()
        .and_then(|stdout| stderr.output_bytes().map(|stderr| (stdout, stderr)))
        .map_err(|error| format!("collect {invocation} output: {error}"))
}

fn append_output_diagnostics(
    message: String,
    streams: Result<(Vec<u8>, Vec<u8>), String>,
) -> String {
    match streams {
        Ok((stdout, stderr)) => format!(
            "{message}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&bounded_diagnostics(&stdout)),
            String::from_utf8_lossy(&bounded_diagnostics(&stderr))
        ),
        Err(error) => format!("{message}\noutput collection failed: {error}"),
    }
}

struct StopReadersOnDrop<'a>(&'a AtomicBool);

impl Drop for StopReadersOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[cfg(unix)]
fn own_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn own_process_group(_command: &mut Command) {}

fn terminate_owned_process_tree(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        let process_group = -(child.id() as libc::pid_t);
        libc::kill(process_group, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Run node --test with a deadline for the child and its captured pipes.
pub(crate) fn run_node_tests(
    node: &Path,
    files: &[std::path::PathBuf],
    timeout: Duration,
) -> Result<Output, String> {
    let mut args = Vec::with_capacity(files.len() + 1);
    args.push(OsString::from("--test"));
    args.extend(files.iter().map(|file| file.as_os_str().to_owned()));
    run_node_process(node, &args, "node --test", timeout)
}

pub(crate) fn run_node_program(
    node: &Path,
    program: &Path,
    timeout: Duration,
) -> Result<Output, String> {
    let args = [program.as_os_str().to_owned()];
    run_node_process(node, &args, "node", timeout)
}

fn run_node_process(
    node: &Path,
    args: &[OsString],
    invocation: &str,
    timeout: Duration,
) -> Result<Output, String> {
    let deadline = Instant::now() + timeout;
    let mut command = Command::new(node);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    own_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("spawn {invocation}: {error}"))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let _stop_readers = StopReadersOnDrop(&stop);
        let stdout_reader = scope.spawn(|| read_pipe(stdout, &stop));
        let stderr_reader = scope.spawn(|| read_pipe(stderr, &stop));
        let mut status: Option<ExitStatus> = None;

        loop {
            if status.is_none() {
                match child.try_wait() {
                    Ok(child_status) => status = child_status,
                    Err(error) => {
                        stop.store(true, Ordering::Release);
                        terminate_owned_process_tree(&mut child);
                        let streams =
                            join_pipe_readers(stdout_reader, stderr_reader, invocation, true);
                        return Err(append_output_diagnostics(
                            format!("poll {invocation}: {error}"),
                            streams,
                        ));
                    }
                }
            }

            let now = Instant::now();
            if let Some(status) = status
                && stdout_reader.is_finished()
                && stderr_reader.is_finished()
                && now < deadline
            {
                let (stdout, stderr) =
                    join_pipe_readers(stdout_reader, stderr_reader, invocation, !status.success())?;
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            if now >= deadline {
                stop.store(true, Ordering::Release);
                terminate_owned_process_tree(&mut child);
                let streams = join_pipe_readers(stdout_reader, stderr_reader, invocation, true);
                let message = format!("{invocation} timed out after {} s", timeout.as_secs_f64());
                #[cfg(unix)]
                let message = format!("{message}\nNODE_PROCESS_GROUP_ID={}", child.id());
                return Err(append_output_diagnostics(message, streams));
            }

            thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    })
}

#[cfg(test)]
mod tests;
