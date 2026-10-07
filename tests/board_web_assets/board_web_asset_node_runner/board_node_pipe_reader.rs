use std::{
    io::{self, Read},
    process::{ChildStderr, ChildStdout},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use super::board_node_output_capture::NodeOutputCapture;

const FINAL_DRAIN: Duration = Duration::from_millis(250);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(super) trait NodePipe: Read {
    #[cfg(unix)]
    fn descriptor(&self) -> std::os::fd::RawFd;
    #[cfg(windows)]
    fn handle(&self) -> std::os::windows::io::RawHandle;
}

macro_rules! node_pipe {
    ($pipe:ty) => {
        impl NodePipe for $pipe {
            #[cfg(unix)]
            fn descriptor(&self) -> std::os::fd::RawFd {
                std::os::fd::AsRawFd::as_raw_fd(self)
            }
            #[cfg(windows)]
            fn handle(&self) -> std::os::windows::io::RawHandle {
                std::os::windows::io::AsRawHandle::as_raw_handle(self)
            }
        }
    };
}
node_pipe!(ChildStdout);
node_pipe!(ChildStderr);

pub(super) fn read_pipe(
    mut pipe: impl NodePipe,
    stop: &AtomicBool,
) -> io::Result<NodeOutputCapture> {
    prepare_pipe(&pipe)?;
    let mut captured = NodeOutputCapture::new()?;
    let mut buffer = [0; 16 * 1024];
    let mut drain_deadline = None;
    loop {
        if stop.load(Ordering::Acquire) {
            let deadline = drain_deadline.get_or_insert_with(|| Instant::now() + FINAL_DRAIN);
            if Instant::now() >= *deadline {
                captured.finish(true)?;
                return Ok(captured);
            }
        }
        match read_available(&mut pipe, &mut buffer) {
            Ok(0) => {
                captured.finish(false)?;
                return Ok(captured);
            }
            Ok(length) => captured.push(&buffer[..length])?,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL_INTERVAL),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn prepare_pipe(pipe: &impl NodePipe) -> io::Result<()> {
    // The pipe is owned by this reader; changing its flags cannot affect a writer.
    let result = unsafe {
        let flags = libc::fcntl(pipe.descriptor(), libc::F_GETFL);
        if flags < 0 {
            -1
        } else {
            libc::fcntl(pipe.descriptor(), libc::F_SETFL, flags | libc::O_NONBLOCK)
        }
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn prepare_pipe(_pipe: &impl NodePipe) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn read_available(pipe: &mut impl NodePipe, buffer: &mut [u8]) -> io::Result<usize> {
    pipe.read(buffer)
}

#[cfg(windows)]
fn read_available(pipe: &mut impl NodePipe, buffer: &mut [u8]) -> io::Result<usize> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn PeekNamedPipe(
            handle: *mut std::ffi::c_void,
            buffer: *mut std::ffi::c_void,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            left: *mut u32,
        ) -> i32;
    }
    let mut available = 0;
    let result = unsafe {
        PeekNamedPipe(
            pipe.handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(109) {
            Ok(0)
        } else {
            Err(error)
        };
    }
    if available == 0 {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    let length = buffer.len().min(available as usize);
    pipe.read(&mut buffer[..length])
}

#[cfg(not(any(unix, windows)))]
fn read_available(_pipe: &mut impl NodePipe, _buffer: &mut [u8]) -> io::Result<usize> {
    Err(io::ErrorKind::Unsupported.into())
}
