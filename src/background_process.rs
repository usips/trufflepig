use std::os::unix::process::CommandExt;
use std::{
    io,
    process::{Child, Command, Stdio},
};

/// Spawn a daemon in its own process group with no inherited standard streams.
pub(crate) fn spawn_background(command: &mut Command) -> io::Result<Child> {
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Reclaims exited background daemons and coordinators spawned by this process.
pub(crate) fn reap_children() {
    loop {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::spawn_background;
    use std::process::Command;

    #[test]
    fn background_child_owns_process_group() {
        let mut command = Command::new("sleep");
        command.arg("60");
        let mut child = spawn_background(&mut command).expect("spawn background child");
        let child_pid = child.id() as libc::pid_t;
        let child_group = unsafe { libc::getpgid(child_pid) };
        let caller_group = unsafe { libc::getpgrp() };

        let result = || {
            assert_eq!(child_group, child_pid);
            assert_ne!(child_group, caller_group);
        };
        result();
        child.kill().expect("kill owned child");
        child.wait().expect("wait for owned child");
    }
}
