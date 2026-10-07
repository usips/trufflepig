use std::{
    ffi::OsString,
    io::{self, Read},
    path::Path,
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

fn read_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_pipe_reader(
    reader: JoinHandle<io::Result<Vec<u8>>>,
    invocation: &str,
    stream: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("{invocation} {stream} reader panicked"))?
        .map_err(|error| format!("read {invocation} {stream}: {error}"))
}

fn join_pipe_readers(
    stdout: JoinHandle<io::Result<Vec<u8>>>,
    stderr: JoinHandle<io::Result<Vec<u8>>>,
    invocation: &str,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let stdout = join_pipe_reader(stdout, invocation, "stdout")?;
    let stderr = join_pipe_reader(stderr, invocation, "stderr")?;
    Ok((stdout, stderr))
}

fn append_output_diagnostics(
    message: String,
    streams: Result<(Vec<u8>, Vec<u8>), String>,
) -> String {
    match streams {
        Ok((stdout, stderr)) => format!(
            "{message}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        ),
        Err(error) => format!("{message}\noutput collection failed: {error}"),
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

/// Run node --test until the child exits and both captured pipes reach EOF.
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
    let stdout_reader = thread::spawn(move || read_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_pipe(stderr));
    let mut status: Option<ExitStatus> = None;

    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(child_status) => status = child_status,
                Err(error) => {
                    terminate_owned_process_tree(&mut child);
                    let streams = join_pipe_readers(stdout_reader, stderr_reader, invocation);
                    return Err(append_output_diagnostics(
                        format!("poll {invocation}: {error}"),
                        streams,
                    ));
                }
            }
        }

        let now = Instant::now();
        if status.is_some() && stdout_reader.is_finished() && stderr_reader.is_finished() {
            if now < deadline {
                let (stdout, stderr) = join_pipe_readers(stdout_reader, stderr_reader, invocation)?;
                return Ok(Output {
                    status: status.expect("checked above"),
                    stdout,
                    stderr,
                });
            }
        }
        if now >= deadline {
            terminate_owned_process_tree(&mut child);
            let streams = join_pipe_readers(stdout_reader, stderr_reader, invocation);
            return Err(append_output_diagnostics(
                format!("{invocation} timed out after {} s", timeout.as_secs()),
                streams,
            ));
        }

        thread::sleep((deadline - now).min(Duration::from_millis(10)));
    }
}
